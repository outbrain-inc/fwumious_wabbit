use core::arch::x86_64::*;
use std::any::Any;
use std::error::Error;
use std::io;
use std::mem::{self, MaybeUninit};
use std::simd::{f32x4, SimdFloat, StdFloat};
use std::sync::Mutex;

use merand48::*;

use optimizer::OptimizerTrait;
use regressor::BlockTrait;

use crate::block_helpers;
use crate::block_helpers::OptimizerData;
use crate::feature_buffer;
use crate::graph;
use crate::graph::BlockGraph;
use crate::model_instance;
use crate::optimizer;
use crate::port_buffer;
use crate::regressor;

const FFM_STACK_BUF_LEN: usize = 131072;
const FFM_CONTRA_BUF_LEN: usize = 16384;
const STEP: usize = f32x4::LANES;

pub struct BlockFFM<L: OptimizerTrait> {
    pub optimizer_ffm: L,
    pub local_data_ffm_values: Vec<f32>,
    pub ffm_k: u32,
    pub ffm_weights_len: u32,
    pub ffm_num_fields: u32,
    pub field_embedding_len: u32,
    pub weights: Vec<f32>,
    pub optimizer: Vec<OptimizerData<L>>,
    pub output_offset: usize,
    mutex: Mutex<()>,
}

impl<L: OptimizerTrait + 'static> BlockFFM<L> {
    fn set_weights(&mut self, lower_bound: f32, difference: f32) {
        for i in 0..self.ffm_weights_len {
            let w = difference * merand48(i as u64) as f32 + lower_bound;
            self.weights[i as usize] = w;
            self.optimizer[i as usize].optimizer_data = self.optimizer_ffm.initial_data();
        }
    }
}

pub fn new_ffm_block(
    bg: &mut graph::BlockGraph,
    mi: &model_instance::ModelInstance,
) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let block = match mi.optimizer {
        model_instance::Optimizer::AdagradLUT => {
            new_ffm_block_without_weights::<optimizer::OptimizerAdagradLUT>(&mi)
        }
        model_instance::Optimizer::AdagradFlex => {
            new_ffm_block_without_weights::<optimizer::OptimizerAdagradFlex>(&mi)
        }
        model_instance::Optimizer::SGD => {
            new_ffm_block_without_weights::<optimizer::OptimizerSGD>(&mi)
        }
    }
        .unwrap();
    let mut block_outputs = bg.add_node(block, vec![]).unwrap();
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

fn new_ffm_block_without_weights<L: OptimizerTrait + 'static>(
    mi: &model_instance::ModelInstance,
) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    let ffm_num_fields = mi.ffm_fields.len() as u32;
    let mut reg_ffm = BlockFFM::<L> {
        weights: Vec::new(),
        optimizer: Vec::new(),
        ffm_weights_len: 0,
        local_data_ffm_values: Vec::with_capacity(1024),
        ffm_k: mi.ffm_k,
        ffm_num_fields,
        field_embedding_len: mi.ffm_k * ffm_num_fields,
        optimizer_ffm: L::new(),
        output_offset: usize::MAX,
        mutex: Mutex::new(()),
    };

    if mi.ffm_k > 0 {
        reg_ffm.optimizer_ffm.init(
            mi.ffm_learning_rate,
            mi.ffm_power_t,
            mi.ffm_init_acc_gradient,
        );
        // At the end we add "spillover buffer", so we can do modulo only on the base address and add offset
        reg_ffm.ffm_weights_len =
            (1 << mi.ffm_bit_precision) + (mi.ffm_fields.len() as u32 * reg_ffm.ffm_k);
    }

    // Verify that forward pass will have enough stack for temporary buffer
    if reg_ffm.ffm_k as usize * mi.ffm_fields.len() * mi.ffm_fields.len() > FFM_CONTRA_BUF_LEN {
        return Err(format!("FFM_CONTRA_BUF_LEN is {}. It needs to be at least ffm_k * number_of_fields^2. number_of_fields: {}, ffm_k: {}, please recompile with larger constant",
                           FFM_CONTRA_BUF_LEN, mi.ffm_fields.len(), reg_ffm.ffm_k))?;
    }

    Ok(Box::new(reg_ffm))
}

impl<L: OptimizerTrait + 'static> BlockTrait for BlockFFM<L> {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    #[inline(always)]
    fn forward_backward(
        &mut self,
        further_blocks: &mut [Box<dyn BlockTrait>],
        fb: &feature_buffer::FeatureBuffer,
        pb: &mut port_buffer::PortBuffer,
        update: bool,
    ) {
        debug_assert!(self.output_offset != usize::MAX);

        unsafe {
            macro_rules! core_macro {
                (
                $local_data_ffm_values:ident
                ) => {
                    // number of outputs
                    let num_outputs = (self.ffm_num_fields * self.ffm_num_fields) as usize;
                    let myslice = &mut pb.tape[self.output_offset .. (self.output_offset + num_outputs)];
                    myslice.fill(0.0);

                    let mut local_data_ffm_values = $local_data_ffm_values;

                    let ffm_weights = &mut self.weights;

                    let ffmk: u32 = self.ffm_k;
                    let ffmk_as_usize: usize = ffmk as usize;
                    let ffmk_start = ffmk_as_usize % STEP;

                    let ffm_fields_count: u32 = fb.ffm_fields_count;
                    let ffm_fields_count_as_usize: usize = ffm_fields_count as usize;
                    let ffm_fields_count_start = ffm_fields_count_as_usize % STEP;

                    let fc: usize = ffm_fields_count_as_usize * ffmk_as_usize;

                    let mut contra_fields: [f32; FFM_CONTRA_BUF_LEN] = MaybeUninit::uninit().assume_init();

                    /* first prepare two things:
                       - transposed contra vectors in contra_fields -
                           - for each vector we sum up all the features within a field
                           - and at the same time transpose it, so we can later directly multiply them with individual feature embeddings
                       - cache of gradients in local_data_ffm_values
                           - we will use these gradients later in backward pass
                    */

                    _mm_prefetch(mem::transmute::<&f32, &i8>(&contra_fields.get_unchecked(fb.ffm_buffer.get_unchecked(0).contra_field_index as usize)), _MM_HINT_T0);
                    let mut ffm_buffer_index = 0;
                    for field_index in 0..ffm_fields_count {
                        let field_index_ffmk = field_index * ffmk;
                        // first we handle fields with no features
                        if ffm_buffer_index >= fb.ffm_buffer.len() ||
                            fb.ffm_buffer.get_unchecked(ffm_buffer_index).contra_field_index > field_index_ffmk
                        {
                            let mut offset: usize = field_index_ffmk as usize;
                            for z in 0..ffm_fields_count_as_usize {
                                for k in offset..offset + ffmk_start {
                                    *contra_fields.get_unchecked_mut(k) = 0.0;
                                }
                                let zeroes_simd = f32x4::splat(0.0);
                                let zeroes = zeroes_simd.as_array();
                                for k in (offset + ffmk_start..offset + ffmk_as_usize).step_by(STEP) {
                                    contra_fields.get_unchecked_mut(k..k + STEP).copy_from_slice(zeroes);
                                }

                                offset += fc;
                            }
                            continue;
                        }

                        let mut feature_num = 0;
                        while ffm_buffer_index < fb.ffm_buffer.len() && fb.ffm_buffer.get_unchecked(ffm_buffer_index).contra_field_index == field_index_ffmk {
                            _mm_prefetch(mem::transmute::<&f32, &i8>(&ffm_weights.get_unchecked(fb.ffm_buffer.get_unchecked(ffm_buffer_index + 1).hash as usize)), _MM_HINT_T0);

                            let feature = fb.ffm_buffer.get_unchecked(ffm_buffer_index);
                            let feature_value = feature.value as f32;
                            let feature_value_simd = f32x4::splat(feature_value);

                            let mut feature_index = feature.hash as usize;
                            let mut offset: usize = field_index_ffmk as usize;

                            if feature_num == 0 {
                                for z in 0..ffm_fields_count_as_usize {
                                    _mm_prefetch(mem::transmute::<&f32, &i8>(&ffm_weights.get_unchecked(feature_index + ffmk_as_usize)), _MM_HINT_T0);
                                    for k in 0..ffmk_start {
                                        *contra_fields.get_unchecked_mut(offset + k) = ffm_weights.get_unchecked(feature_index + k) * feature_value;
                                    }
                                    for k in (ffmk_start..ffmk_as_usize).step_by(STEP) {
                                        let ffm_weights_simd = f32x4::from_slice(ffm_weights.get_unchecked(feature_index + k..feature_index + k + STEP));
                                        let result_simd = (feature_value_simd * ffm_weights_simd);
                                        contra_fields.get_unchecked_mut(offset + k..offset + k + STEP).copy_from_slice(result_simd.as_array());
                                    }

                                    offset += fc;
                                    feature_index += ffmk_as_usize;
                                }
                            } else {
                                for z in 0..ffm_fields_count_as_usize {
                                    _mm_prefetch(mem::transmute::<&f32, &i8>(&ffm_weights.get_unchecked(feature_index + ffmk_as_usize)), _MM_HINT_T0);
                                    for k in 0..ffmk_start {
                                        *contra_fields.get_unchecked_mut(offset + k) += ffm_weights.get_unchecked(feature_index + k) * feature_value;
                                    }
                                    for k in (ffmk_start..ffmk_as_usize).step_by(STEP) {
                                        let ffm_weights_simd = f32x4::from_slice(ffm_weights.get_unchecked(feature_index + k..feature_index + k + STEP));
                                        let contra_fields_simd = f32x4::from_slice(contra_fields.get_unchecked(offset + k..offset + k + STEP));
                                        let result_simd = (feature_value_simd * ffm_weights_simd + contra_fields_simd);
                                        contra_fields.get_unchecked_mut(offset + k..offset + k + STEP).copy_from_slice(result_simd.as_array());
                                    }

                                    offset += fc;
                                    feature_index += ffmk_as_usize;
                                }
                            }

                            ffm_buffer_index += 1;
                            feature_num += 1;
                        }
                    }

                    let mut ffm_values_offset = 0;
                    for (i, feature) in fb.ffm_buffer.iter().enumerate() {
                        let feature_value = feature.value;
                        let feature_value_simd = f32x4::splat(feature_value);
                        let feature_index = feature.hash as usize;
                        let feature_contra_field_index = feature.contra_field_index as usize;

                        let contra_offset = feature_contra_field_index * ffm_fields_count_as_usize;

                        let contra_offset2 = contra_offset / ffmk_as_usize;

                        let mut vv = 0;
                        for z in 0..ffm_fields_count_as_usize {
                            let mut correction = 0.0;
                            let mut correction_simd = f32x4::splat(0.0);

                            let vv_feature_index = feature_index + vv;
                            let vv_contra_offset = contra_offset + vv;

                            if vv == feature_contra_field_index {
                                for k in 0..ffmk_start {
                                    let ffm_weight = ffm_weights.get_unchecked(vv_feature_index + k);
                                    let contra_weight = *contra_fields.get_unchecked(vv_contra_offset + k) - ffm_weight * feature_value;
                                    let gradient = feature_value * contra_weight;
                                    *local_data_ffm_values.get_unchecked_mut(ffm_values_offset + k) = gradient;

                                    correction += ffm_weight * gradient;
                                }

                                for k in (ffmk_start..ffmk_as_usize).step_by(STEP) {
                                    let ffm_weight_simd = f32x4::from_slice(ffm_weights.get_unchecked(vv_feature_index + k..vv_feature_index + k + STEP));

                                    let contra_weight_simd = f32x4::from_slice(contra_fields
                                        .get_unchecked(vv_contra_offset + k..vv_contra_offset + k + STEP)) - ffm_weight_simd * feature_value_simd;
                                    let gradient_simd = feature_value_simd * contra_weight_simd;

                                    local_data_ffm_values.get_unchecked_mut(ffm_values_offset + k..ffm_values_offset + k + STEP)
                                        .copy_from_slice(gradient_simd.as_array());

                                    correction_simd += ffm_weight_simd * gradient_simd;
                                }
                            } else {
                                for k in 0..ffmk_start {
                                    let contra_weight = *contra_fields.get_unchecked(vv_contra_offset + k);
                                    let gradient = feature_value * contra_weight;

                                    *local_data_ffm_values.get_unchecked_mut(ffm_values_offset + k) = gradient;

                                    let ffm_weight = ffm_weights.get_unchecked(vv_feature_index + k);
                                    correction += ffm_weight * gradient;
                                }

                                for k in (ffmk_start..ffmk_as_usize).step_by(STEP) {
                                    let contra_weight_simd = f32x4::from_slice(contra_fields
                                        .get_unchecked(vv_contra_offset + k..vv_contra_offset + k + STEP));
                                    let gradient_simd = feature_value_simd * contra_weight_simd;

                                    local_data_ffm_values.get_unchecked_mut(ffm_values_offset + k..ffm_values_offset + k + STEP)
                                        .copy_from_slice(gradient_simd.as_array());

                                    let ffm_weight_simd = f32x4::from_slice(ffm_weights.get_unchecked(vv_feature_index + k..vv_feature_index + k + STEP));
                                    correction_simd += ffm_weight_simd * gradient_simd;
                                }
                            }
                            correction += correction_simd.reduce_sum();

                            *myslice.get_unchecked_mut(contra_offset2 + z) += correction * 0.5;
                            vv += ffmk_as_usize;
                            ffm_values_offset += ffmk_as_usize;
                        }
                    }

                    block_helpers::forward_backward(further_blocks, fb, pb, update);

                    if update {
                        let mut local_index: usize = 0;
                        let myslice = &mut pb.tape[self.output_offset..(self.output_offset + num_outputs)];

                        for feature in &fb.ffm_buffer {
                            let mut feature_index = feature.hash as usize;
                            let contra_offset = (feature.contra_field_index * fb.ffm_fields_count) as usize / ffmk_as_usize;

                            for z in 0..ffm_fields_count_as_usize {
                                let general_gradient = myslice.get_unchecked(contra_offset + z);

                                for k in 0.. ffmk_start {
                                    let feature_value = *local_data_ffm_values.get_unchecked(local_index);
                                    let gradient = general_gradient * feature_value;
                                    let update = self.optimizer_ffm.calculate_update(gradient,
                                        &mut self.optimizer.get_unchecked_mut(feature_index).optimizer_data);

                                    *ffm_weights.get_unchecked_mut(feature_index) -= update;
                                    local_index += 1;
                                    feature_index += 1;
                                }

                                let general_gradient_simd = f32x4::splat(*general_gradient);
                                for k in (ffmk_start..ffmk_as_usize).step_by(STEP) {
                                    let feature_value_simd = f32x4::from_slice(local_data_ffm_values.get_unchecked(local_index..local_index + STEP));
                                    let gradient_simd = general_gradient_simd * feature_value_simd;
                                    let gradient = gradient_simd.as_array();

                                    let update = self.optimizer_ffm.calculate_update(gradient[0],
                                        &mut self.optimizer.get_unchecked_mut(feature_index).optimizer_data);
                                    let update_1 = self.optimizer_ffm.calculate_update(gradient[1],
                                        &mut self.optimizer.get_unchecked_mut(feature_index + 1).optimizer_data);
                                    let update_2 = self.optimizer_ffm.calculate_update(gradient[2],
                                        &mut self.optimizer.get_unchecked_mut(feature_index + 2).optimizer_data);
                                    let update_3 = self.optimizer_ffm.calculate_update(gradient[3],
                                        &mut self.optimizer.get_unchecked_mut(feature_index + 3).optimizer_data);

                                    let update_simd = f32x4::from_array([update, update_1, update_2, update_3]);
                                    let ffm_weights_simd = f32x4::from_slice(ffm_weights.get_unchecked(feature_index..feature_index + STEP));
                                    let result_simd = ffm_weights_simd - update_simd;

                                    ffm_weights.get_unchecked_mut(feature_index..feature_index+STEP).copy_from_slice(result_simd.as_array());
                                    local_index += STEP;
                                    feature_index += STEP;
                                }
                            }
                        }
                    }
                    // The only exit point
                    return
                }
            } // End of macro

            let local_data_ffm_len = fb.ffm_buffer.len() * (self.ffm_k * fb.ffm_fields_count) as usize;
            if local_data_ffm_len < FFM_STACK_BUF_LEN {
                // Fast-path - using on-stack data structures
                let mut local_data_ffm_values: [f32; FFM_STACK_BUF_LEN as usize] =
                    MaybeUninit::uninit().assume_init();
                core_macro!(local_data_ffm_values);
            } else {
                // Slow-path - using heap data structures
                log::warn!("FFM data too large, allocating on the heap (slow path)!");
                let guard = self.mutex.lock().unwrap(); // following operations are not thread safe
                if local_data_ffm_len > self.local_data_ffm_values.len() {
                    self.local_data_ffm_values
                        .reserve(local_data_ffm_len - self.local_data_ffm_values.len() + 1024);
                }
                let mut local_data_ffm_values = &mut self.local_data_ffm_values;

                core_macro!(local_data_ffm_values);
            }
        } // unsafe end
    }

    fn forward(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &feature_buffer::FeatureBuffer,
        pb: &mut port_buffer::PortBuffer,
    ) {
        debug_assert!(self.output_offset != usize::MAX);

        let num_outputs = (self.ffm_num_fields * self.ffm_num_fields) as usize;
        let myslice = &mut pb.tape[self.output_offset..(self.output_offset + num_outputs)];
        myslice.fill(0.0);

        unsafe {
            let ffm_weights = &self.weights;
            _mm_prefetch(
                mem::transmute::<&f32, &i8>(
                    &ffm_weights
                        .get_unchecked(fb.ffm_buffer.get_unchecked(0).hash as usize),
                ),
                _MM_HINT_T0,
            );

            /* We first prepare "contra_fields" or collapsed field embeddings, where we sum all individual feature embeddings
               We need to be careful to:
               - handle fields with zero features present
               - handle values on diagonal - we want to be able to exclude self-interactions later (we pre-substract from wsum)
               - optimize for just copying the embedding over when looking at first feature of the field, and add embeddings for the rest
               - optimize for very common case of value of the feature being 1.0 - avoid multiplications
             */

            let ffmk: u32 = self.ffm_k;
            let ffmk_as_usize: usize = ffmk as usize;

            let ffmk_end = ffmk_as_usize - ffmk_as_usize % STEP;

            let ffm_fields_count: u32 = fb.ffm_fields_count;
            let ffm_fields_count_as_usize: usize = ffm_fields_count as usize;
            let ffm_fields_count_plus_one = ffm_fields_count + 1;

            let field_embedding_len_as_usize = self.field_embedding_len as usize;
            let field_embedding_len_end = field_embedding_len_as_usize - field_embedding_len_as_usize % STEP;

            let mut contra_fields: [f32; FFM_CONTRA_BUF_LEN] = MaybeUninit::uninit().assume_init();

            let mut ffm_buffer_index = 0;

            let zeroes: [f32; STEP] = [0.0; STEP];

            for field_index in 0..ffm_fields_count {
                let field_index_ffmk = field_index * ffmk;
                let field_index_ffmk_as_usize = field_index_ffmk as usize;
                let offset = (field_index_ffmk * ffm_fields_count) as usize;
                // first we handle fields with no features
                if ffm_buffer_index >= fb.ffm_buffer.len()
                    || fb.ffm_buffer.get_unchecked(ffm_buffer_index).contra_field_index > field_index_ffmk
                {
                    // first feature of the field - just overwrite
                    for z in (offset..offset + field_embedding_len_end).step_by(STEP) {
                        contra_fields.get_unchecked_mut(z..z + STEP).copy_from_slice(&zeroes);
                    }

                    for z in offset + field_embedding_len_end..offset + field_embedding_len_as_usize {
                        *contra_fields.get_unchecked_mut(z) = 0.0;
                    }

                    continue;
                }

                let mut feature_num = 0;
                while ffm_buffer_index < fb.ffm_buffer.len()
                    && fb.ffm_buffer.get_unchecked(ffm_buffer_index).contra_field_index == field_index_ffmk
                {
                    _mm_prefetch(
                        mem::transmute::<&f32, &i8>(
                            &ffm_weights.get_unchecked(
                                fb.ffm_buffer.get_unchecked(ffm_buffer_index + 1).hash as usize),
                        ),
                        _MM_HINT_T0,
                    );
                    let feature = fb.ffm_buffer.get_unchecked(ffm_buffer_index);
                    let feature_index = feature.hash as usize;
                    let feature_value = feature.value;
                    let feature_value_simd = f32x4::splat(feature_value);

                    if feature_num == 0 {
                        // first feature of the field - just overwrite
                        for z in (0..field_embedding_len_end).step_by(STEP) {
                            let ffm_weights_simd = f32x4::from_slice(ffm_weights.get_unchecked(feature_index + z..feature_index + z + STEP));
                            let result_simd = feature_value_simd * ffm_weights_simd;
                            contra_fields.get_unchecked_mut(offset + z..offset + z + STEP).copy_from_slice(result_simd.as_array());
                        }
                        for z in field_embedding_len_end..field_embedding_len_as_usize {
                            *contra_fields.get_unchecked_mut(offset + z) =
                                ffm_weights.get_unchecked(feature_index + z) * feature_value;
                        }
                    } else {
                        for z in (0..field_embedding_len_end).step_by(STEP) {
                            let ffm_weights_simd = f32x4::from_slice(ffm_weights.get_unchecked(feature_index + z..feature_index + z + STEP));
                            let contra_fields_simd = f32x4::from_slice(contra_fields.get_unchecked(offset + z..offset + z + STEP));
                            let result_simd = feature_value_simd * ffm_weights_simd + contra_fields_simd;
                            contra_fields.get_unchecked_mut(offset + z..offset + z + STEP).copy_from_slice(result_simd.as_array());
                        }
                        for z in field_embedding_len_end..field_embedding_len_as_usize {
                            *contra_fields.get_unchecked_mut(offset + z) +=
                                ffm_weights.get_unchecked(feature_index + z) * feature_value;
                        }
                    }

                    let feature_field_index = feature_index + field_index_ffmk_as_usize;

                    let (ffm_weights_prefix, ffm_weights_middle, ffm_weights_suffix) = ffm_weights.get_unchecked(feature_field_index..feature_field_index + ffmk_as_usize)
                        .as_simd::<STEP>();

                    let correction_simd = ffm_weights_middle.iter()
                        .fold(f32x4::splat(0.0), |sum, val| sum + (val * val));
                    let correction = ffm_weights_prefix.iter().chain(ffm_weights_suffix)
                        .fold(correction_simd.reduce_sum(), |sum, val| sum + (val * val));

                    *myslice.get_unchecked_mut(((feature.contra_field_index / ffmk) * ffm_fields_count_plus_one) as usize) -=
                        correction * 0.5 * feature_value * feature_value;

                    ffm_buffer_index += 1;
                    feature_num += 1;
                }
            }

            let mut f1_offset = 0;
            let mut f1_index_offset = 0;
            let mut f1_ffmk = 0;
            let mut diagonal_row = 0;
            for f1 in 0..ffm_fields_count_as_usize {
                let mut f1_offset_ffmk = f1_offset + f1_ffmk;

                // Self-interaction
                let (v_prefix, v_middle, v_suffix) = contra_fields.get_unchecked(f1_offset_ffmk..f1_offset_ffmk + ffmk_as_usize)
                    .as_simd::<STEP>();
                let v_simd = v_middle.iter()
                    .fold(f32x4::splat(0.0), |sum, val| sum + (val * val));
                let v = v_prefix.iter().chain(v_suffix)
                    .fold(v_simd.reduce_sum(), |sum, val| sum + (val * val));

                *myslice.get_unchecked_mut(diagonal_row + f1) += v * 0.5;

                let mut f2_index_offset = f1_index_offset + ffm_fields_count_as_usize;
                let mut f2_offset_ffmk = f1_offset + f1_ffmk;
                for f2 in f1 + 1..ffm_fields_count_as_usize {
                    let f1_index = f1_index_offset + f2;
                    let f2_index = f2_index_offset + f1;

                    f1_offset_ffmk += ffmk_as_usize;
                    f2_offset_ffmk += field_embedding_len_as_usize;

                    let (contra_fields_1_prefix, contra_fields_1_middle, contra_fields_1_suffix) = contra_fields
                        .get_unchecked(f1_offset_ffmk..f1_offset_ffmk + ffmk_as_usize)
                        .as_simd::<STEP>();

                    let (contra_fields_2_prefix, contra_fields_2_middle, contra_fields_2_suffix) = contra_fields
                        .get_unchecked(f2_offset_ffmk..f2_offset_ffmk + ffmk_as_usize)
                        .as_simd::<STEP>();

                    let contra_field_simd = contra_fields_1_middle.iter().zip(contra_fields_2_middle.iter())
                        .fold(f32x4::splat(0.0), |sum, val| sum + (val.0 * val.1));
                    let contra_field = contra_fields_1_prefix.iter().chain(contra_fields_1_suffix)
                        .zip(contra_fields_2_prefix.iter().chain(contra_fields_2_suffix))
                        .fold(contra_field_simd.reduce_sum(), |sum, val| sum + (val.0 * val.1))
                        * 0.5;

                    *myslice.get_unchecked_mut(f1_index) += contra_field;
                    *myslice.get_unchecked_mut(f2_index) += contra_field;

                    f2_index_offset += ffm_fields_count_as_usize;
                }

                f1_offset += field_embedding_len_as_usize;
                f1_ffmk += ffmk_as_usize;
                f1_index_offset += ffm_fields_count_as_usize;
                diagonal_row += ffm_fields_count_as_usize;
            }
        }
        block_helpers::forward(further_blocks, fb, pb);
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {
        self.weights = vec![
            0.0;
            self.ffm_weights_len as usize
        ];
        self.optimizer = vec![
            OptimizerData::<L> {
                optimizer_data: self.optimizer_ffm.initial_data(),
            };
            self.ffm_weights_len as usize
        ];

        match mi.ffm_initialization_type.as_str() {
            "default" => {
                if mi.ffm_k > 0 {
                    if mi.ffm_init_width == 0.0 {
                        // Initialization that has showed to work ok for us, like in ffm.pdf, but centered around zero and further divided by 50
                        let ffm_one_over_k_root = 1.0 / (self.ffm_k as f32).sqrt() / 50.0;
                        for i in 0..self.ffm_weights_len {
                            self.weights[i as usize] = (1.0
                                * merand48((self.ffm_weights_len as usize + i as usize) as u64)
                                - 0.5)
                                * ffm_one_over_k_root;
                            self.optimizer[i as usize].optimizer_data =
                                self.optimizer_ffm.initial_data();
                        }
                    } else {
                        let zero_half_band_width = mi.ffm_init_width * mi.ffm_init_zero_band * 0.5;
                        let band_width = mi.ffm_init_width * (1.0 - mi.ffm_init_zero_band);
                        for i in 0..self.ffm_weights_len {
                            let mut w = merand48(i as u64) * band_width - band_width * 0.5;
                            if w > 0.0 {
                                w += zero_half_band_width;
                            } else {
                                w -= zero_half_band_width;
                            }
                            w += mi.ffm_init_center;
                            self.weights[i as usize] = w;
                            self.optimizer[i as usize].optimizer_data =
                                self.optimizer_ffm.initial_data();
                        }
                    }
                }
            }
            _ => {
                panic!("Please select a valid activation function.")
            }
        }
    }

    fn get_serialized_len(&self) -> usize {
        return self.ffm_weights_len as usize;
    }

    fn write_weights_to_buf(
        &self,
        output_bufwriter: &mut dyn io::Write,
    ) -> Result<(), Box<dyn Error>> {
        block_helpers::write_weights_to_buf(&self.weights, output_bufwriter)?;
        block_helpers::write_weights_to_buf(&self.optimizer, output_bufwriter)?;
        Ok(())
    }

    fn read_weights_from_buf(
        &mut self,
        input_bufreader: &mut dyn io::Read,
    ) -> Result<(), Box<dyn Error>> {
        block_helpers::read_weights_from_buf(&mut self.weights, input_bufreader)?;
        block_helpers::read_weights_from_buf(&mut self.optimizer, input_bufreader)?;
        Ok(())
    }

    fn get_num_output_values(&self, output: graph::OutputSlot) -> usize {
        assert_eq!(output.get_output_index(), 0);
        return (self.ffm_num_fields * self.ffm_num_fields) as usize;
    }

    fn get_num_output_slots(&self) -> usize {
        1
    }

    fn set_input_offset(&mut self, input: graph::InputSlot, offset: usize) {
        panic!("You cannot set_input_offset() for BlockFFM");
    }

    fn set_output_offset(&mut self, output: graph::OutputSlot, offset: usize) {
        assert_eq!(output.get_output_index(), 0);
        self.output_offset = offset;
    }

    fn read_weights_from_buf_into_forward_only(
        &self,
        input_bufreader: &mut dyn io::Read,
        forward: &mut Box<dyn BlockTrait>,
    ) -> Result<(), Box<dyn Error>> {
        let mut forward = forward
            .as_any()
            .downcast_mut::<BlockFFM<optimizer::OptimizerSGD>>()
            .unwrap();
        block_helpers::read_weights_from_buf(&mut forward.weights, input_bufreader)?;
        block_helpers::skip_weights_from_buf(
            self.ffm_weights_len as usize,
            &self.optimizer,
            input_bufreader,
        )?;
        Ok(())
    }

    /// Sets internal state of weights based on some completely object-dependent parameters
    fn testing_set_weights(
        &mut self,
        aa: i32,
        bb: i32,
        index: usize,
        w: &[f32],
    ) -> Result<(), Box<dyn Error>> {
        self.weights[index] = w[0];
        self.optimizer[index].optimizer_data = self.optimizer_ffm.initial_data();
        Ok(())
    }
}

mod tests {
    use block_helpers::{slearn2, spredict2};

    use crate::assert_epsilon;
    use crate::block_loss_functions;
    use crate::feature_buffer;
    use crate::feature_buffer::HashAndValueAndSeq;
    use crate::model_instance::Optimizer;
    use crate::vwmap;

    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    fn ffm_vec(
        v: Vec<feature_buffer::HashAndValueAndSeq>,
        ffm_fields_count: u32,
    ) -> feature_buffer::FeatureBuffer {
        feature_buffer::FeatureBuffer {
            label: 0.0,
            example_importance: 1.0,
            example_number: 0,
            lr_buffer: Vec::new(),
            ffm_buffer: v,
            ffm_fields_count,
        }
    }

    fn ffm_init<T: OptimizerTrait + 'static>(block_ffm: &mut Box<dyn BlockTrait>) -> () {
        let mut block_ffm = block_ffm.as_any().downcast_mut::<BlockFFM<T>>().unwrap();

        for i in 0..block_ffm.weights.len() {
            block_ffm.weights[i] = 1.0;
            block_ffm.optimizer[i].optimizer_data = block_ffm.optimizer_ffm.initial_data();
        }
    }

    #[test]
    fn test_ffm_k1() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        mi.learning_rate = 0.1;
        mi.ffm_learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.ffm_power_t = 0.0;
        mi.bit_precision = 18;
        mi.ffm_k = 1;
        mi.ffm_bit_precision = 18;
        mi.ffm_fields = vec![vec![], vec![]]; // This isn't really used
        mi.optimizer = Optimizer::AdagradLUT;

        // Nothing can be learned from a single field in FFMs
        let mut bg = BlockGraph::new();
        let ffm_block = new_ffm_block(&mut bg, &mi).unwrap();
        let loss_block = block_loss_functions::new_logloss_block(&mut bg, ffm_block, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);
        let mut pb = bg.new_port_buffer();

        let fb = ffm_vec(
            vec![HashAndValueAndSeq {
                hash: 1,
                value: 1.0,
                contra_field_index: 0,
            }],
            1,
        ); // saying we have 1 field isn't entirely correct
        assert_epsilon!(spredict2(&mut bg, &fb, &mut pb, true), 0.5);
        assert_epsilon!(slearn2(&mut bg, &fb, &mut pb, true), 0.5);

        // With two fields, things start to happen
        // Since fields depend on initial randomization, these tests are ... peculiar.
        mi.optimizer = Optimizer::AdagradFlex;
        let mut bg = BlockGraph::new();

        let ffm_block = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, ffm_block, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);
        let mut pb = bg.new_port_buffer();

        ffm_init::<optimizer::OptimizerAdagradFlex>(&mut bg.blocks_final[0]);
        let fb = ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 1.0,
                    contra_field_index: mi.ffm_k,
                },
            ],
            2,
        );
        assert_epsilon!(spredict2(&mut bg, &fb, &mut pb, true), 0.7310586);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.7310586);

        assert_epsilon!(spredict2(&mut bg, &fb, &mut pb, true), 0.7024794);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.7024794);

        // Two fields, use values
        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        ffm_init::<optimizer::OptimizerAdagradLUT>(&mut bg.blocks_final[0]);
        let fb = ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 2.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 2.0,
                    contra_field_index: mi.ffm_k * 1,
                },
            ],
            2,
        );
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.98201376);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.98201376);
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.81377685);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.81377685);
    }

    #[test]
    fn test_ffm_k4() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        mi.learning_rate = 0.1;
        mi.ffm_learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.ffm_power_t = 0.0;
        mi.ffm_k = 4;
        mi.ffm_bit_precision = 18;
        mi.ffm_fields = vec![vec![], vec![]]; // This isn't really used

        // Nothing can be learned from a single field in FFMs
        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        let fb = ffm_vec(
            vec![HashAndValueAndSeq {
                hash: 1,
                value: 1.0,
                contra_field_index: 0,
            }],
            1,
        );
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.5);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.5);
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.5);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.5);

        // With two fields, things start to happen
        // Since fields depend on initial randomization, these tests are ... peculiar.
        mi.optimizer = Optimizer::AdagradFlex;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        ffm_init::<optimizer::OptimizerAdagradFlex>(&mut bg.blocks_final[0]);
        let fb = ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 1.0,
                    contra_field_index: mi.ffm_k * 1,
                },
            ],
            2,
        );
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.98201376);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.98201376);
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.96277946);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.96277946);

        // Two fields, use values
        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        ffm_init::<optimizer::OptimizerAdagradLUT>(&mut bg.blocks_final[0]);
        let fb = ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 2.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 2.0,
                    contra_field_index: mi.ffm_k * 1,
                },
            ],
            2,
        );
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.9999999);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.9999999);
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.99685884);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.99685884);
    }

    #[test]
    fn test_ffm_multivalue() {
        let vw_map_string = r#"
A,featureA
B,featureB
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        mi.learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.ffm_k = 1;
        mi.ffm_bit_precision = 18;
        mi.ffm_power_t = 0.0;
        mi.ffm_learning_rate = 0.1;
        mi.ffm_fields = vec![vec![], vec![]];

        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        let mut p: f32;

        ffm_init::<optimizer::OptimizerAdagradLUT>(&mut bg.blocks_final[0]);
        let fbuf = &ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 3 * 1000,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 2.0,
                    contra_field_index: mi.ffm_k * 1,
                },
            ],
            2,
        );
        assert_epsilon!(spredict2(&mut bg, &fbuf, &mut pb, true), 0.9933072);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, true), 0.9933072);
        assert_epsilon!(spredict2(&mut bg, &fbuf, &mut pb, false), 0.9395168);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, false), 0.9395168);
        assert_epsilon!(spredict2(&mut bg, &fbuf, &mut pb, false), 0.9395168);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, false), 0.9395168);
    }

    #[test]
    fn test_ffm_multivalue_k4_nonzero_powert() {
        let vw_map_string = r#"
A,featureA
B,featureB
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        mi.ffm_k = 4;
        mi.ffm_bit_precision = 18;
        mi.ffm_fields = vec![vec![], vec![]];

        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        ffm_init::<optimizer::OptimizerAdagradLUT>(&mut bg.blocks_final[0]);
        let fbuf = &ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 3 * 1000,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 2.0,
                    contra_field_index: mi.ffm_k * 1,
                },
            ],
            2,
        );

        assert_eq!(spredict2(&mut bg, &fbuf, &mut pb, true), 1.0);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, true), 1.0);
        assert_eq!(spredict2(&mut bg, &fbuf, &mut pb, false), 0.9949837);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, false), 0.9949837);
        assert_eq!(slearn2(&mut bg, &fbuf, &mut pb, false), 0.9949837);
    }

    #[test]
    fn test_ffm_missing_field() {
        // This test is useful to check if we don't by accient forget to initialize any of the collapsed
        // embeddings for the field, when field has no instances of a feature in it
        // We do by having three-field situation where only the middle field has features
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        mi.learning_rate = 0.1;
        mi.ffm_learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.ffm_power_t = 0.0;
        mi.bit_precision = 18;
        mi.ffm_k = 1;
        mi.ffm_bit_precision = 18;
        mi.ffm_fields = vec![vec![], vec![], vec![]]; // This isn't really used

        // Nothing can be learned from a single field in FFMs
        mi.optimizer = Optimizer::AdagradLUT;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        // With two fields, things start to happen
        // Since fields depend on initial randomization, these tests are ... peculiar.
        mi.optimizer = Optimizer::AdagradFlex;
        let mut bg = BlockGraph::new();
        let re_ffm = new_ffm_block(&mut bg, &mi).unwrap();
        let lossf = block_loss_functions::new_logloss_block(&mut bg, re_ffm, true);
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        ffm_init::<optimizer::OptimizerAdagradFlex>(&mut bg.blocks_final[0]);
        let fb = ffm_vec(
            vec![
                HashAndValueAndSeq {
                    hash: 1,
                    value: 1.0,
                    contra_field_index: 0,
                },
                HashAndValueAndSeq {
                    hash: 5,
                    value: 1.0,
                    contra_field_index: mi.ffm_k * 1,
                },
                HashAndValueAndSeq {
                    hash: 100,
                    value: 1.0,
                    contra_field_index: mi.ffm_k * 2,
                },
            ],
            3,
        );
        assert_epsilon!(spredict2(&mut bg, &fb, &mut pb, true), 0.95257413);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, false), 0.95257413);

        // here we intentionally have just the middle field
        let fb = ffm_vec(
            vec![HashAndValueAndSeq {
                hash: 5,
                value: 1.0,
                contra_field_index: mi.ffm_k * 1,
            }],
            3,
        );
        assert_eq!(spredict2(&mut bg, &fb, &mut pb, true), 0.5);
        assert_eq!(slearn2(&mut bg, &fb, &mut pb, true), 0.5);
    }
}
