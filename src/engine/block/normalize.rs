use std::any::Any;
use std::error::Error;

use crate::engine::block::iterators;
use crate::namespace::feature_buffer::FeatureBuffer;
use crate::engine::graph;
use crate::model_instance;
use crate::engine::port_buffer::PortBuffer;
use crate::engine::regressor::BlockCache;
use crate::engine::regressor::BlockTrait;

const EPS: f32 = 1e-2;

pub struct BlockNormalize {
    pub num_inputs: usize,
    pub input_offset: usize,
    pub output_offset: usize,
}

// This is purely variance normalization as described in
// https://arxiv.org/pdf/2006.12753.pdf
// Early results show no improvements for normalization od neural layers

pub fn new_normalize_layer_block(
    bg: &mut graph::BlockGraph,
    _mi: &model_instance::ModelInstance,
    input: graph::BlockPtrOutput,
) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let num_inputs = bg.get_num_output_values(vec![&input]);
    assert_ne!(num_inputs, 0);
    let block = Box::new(BlockNormalize {
        output_offset: usize::MAX,
        input_offset: usize::MAX,
        num_inputs,
    });
    let mut block_outputs = bg.add_node(block, vec![input])?;
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

impl BlockTrait for BlockNormalize {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn get_num_output_values(&self, output: graph::OutputSlot) -> usize {
        assert_eq!(output.get_output_index(), 0);
        return self.num_inputs;
    }

    fn set_input_offset(&mut self, input: graph::InputSlot, offset: usize) {
        assert_eq!(input.get_input_index(), 0);
        self.input_offset = offset;
    }

    fn set_output_offset(&mut self, output: graph::OutputSlot, offset: usize) {
        assert_eq!(output.get_output_index(), 0);
        self.output_offset = offset;
    }

    #[inline(always)]
    fn forward_backward(
        &mut self,
        further_blocks: &mut [Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
        update: bool,
    ) {
        debug_assert!(self.output_offset != usize::MAX);
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        unsafe {
            let mut mean: f32 = 0.0;
            for i in 0..self.num_inputs {
                mean += *pb.tape.get_unchecked_mut(self.input_offset + i);
            }
            mean /= self.num_inputs as f32;
            let meansq = mean * mean;
            let mut variance: f32 = 0.0;
            for i in 0..self.num_inputs {
                let w = meansq - *pb.tape.get_unchecked_mut(self.input_offset + i);
                variance += w * w;
            }
            variance += EPS;
            variance /= self.num_inputs as f32;
            variance = variance.sqrt();

            let variance_inv = 1.0 / variance;

            for i in 0..self.num_inputs {
                *pb.tape.get_unchecked_mut(self.output_offset + i) =
                    (*pb.tape.get_unchecked(self.input_offset + i) - mean) * variance_inv;
            }
            iterators::forward_backward(further_blocks, fb, pb, update);

            if update {
                for i in 0..self.num_inputs {
                    *pb.tape.get_unchecked_mut(self.input_offset + i) =
                        *pb.tape.get_unchecked_mut(self.output_offset + i) * variance_inv;
                }
            }
        }
    }

    fn forward(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
    ) {
        self.internal_forward(pb);
        iterators::forward(further_blocks, fb, pb);
    }

    fn forward_with_cache(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
        caches: &[BlockCache],
    ) {
        self.internal_forward(pb);
        iterators::forward_with_cache(further_blocks, fb, pb, caches);
    }
}

impl BlockNormalize {
    #[inline(always)]
    fn internal_forward(&self, pb: &mut PortBuffer) -> f32 {
        debug_assert!(self.output_offset != usize::MAX);
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        unsafe {
            let mut mean: f32 = 0.0;
            for i in 0..self.num_inputs {
                mean += *pb.tape.get_unchecked_mut(self.input_offset + i);
            }
            mean /= self.num_inputs as f32;
            let meansq = mean * mean;
            let mut variance: f32 = 0.0;
            for i in 0..self.num_inputs {
                let w = meansq - *pb.tape.get_unchecked_mut(self.input_offset + i);
                variance += w * w;
            }
            variance += EPS;
            variance /= self.num_inputs as f32;
            variance = variance.sqrt();

            let variance_inv = 1.0 / variance;

            for i in 0..self.num_inputs {
                *pb.tape.get_unchecked_mut(self.output_offset + i) =
                    *pb.tape.get_unchecked(self.input_offset + i) * variance_inv;
            }

            variance_inv
        }
    }
}

pub struct BlockStopBackward {
    pub num_inputs: usize,
    pub input_offset: usize,
    pub output_offset: usize,
}

// This is purely variance normalization as described in
// https://arxiv.org/pdf/2006.12753.pdf
// Early results show no improvements for normalization od neural layers

pub fn new_stop_block(
    bg: &mut graph::BlockGraph,
    _mi: &model_instance::ModelInstance,
    input: graph::BlockPtrOutput,
) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let num_inputs = bg.get_num_output_values(vec![&input]);
    debug_assert!(num_inputs != 0);
    let block = Box::new(BlockStopBackward {
        output_offset: usize::MAX,
        input_offset: usize::MAX,
        num_inputs,
    });
    let mut block_outputs = bg.add_node(block, vec![input])?;
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

impl BlockTrait for BlockStopBackward {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, _mi: &model_instance::ModelInstance) {}

    fn get_num_output_values(&self, output: graph::OutputSlot) -> usize {
        assert_eq!(output.get_output_index(), 0);
        return self.num_inputs;
    }

    fn set_input_offset(&mut self, input: graph::InputSlot, offset: usize) {
        assert_eq!(input.get_input_index(), 0);
        self.input_offset = offset;
    }

    fn set_output_offset(&mut self, output: graph::OutputSlot, offset: usize) {
        assert_eq!(output.get_output_index(), 0);
        self.output_offset = offset;
    }

    #[inline(always)]
    fn forward_backward(
        &mut self,
        further_blocks: &mut [Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
        update: bool,
    ) {
        self.internal_forward(pb);

        iterators::forward_backward(further_blocks, fb, pb, update);

        if update {
            pb.tape[self.input_offset..(self.input_offset + self.num_inputs)].fill(0.0);
        }
    }

    fn forward(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
    ) {
        self.internal_forward(pb);
        iterators::forward(further_blocks, fb, pb);
    }

    fn forward_with_cache(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &FeatureBuffer,
        pb: &mut PortBuffer,
        caches: &[BlockCache],
    ) {
        self.internal_forward(pb);
        iterators::forward_with_cache(further_blocks, fb, pb, caches);
    }
}

impl BlockStopBackward {
    #[inline(always)]
    fn internal_forward(&self, pb: &mut PortBuffer) {
        debug_assert!(self.output_offset != usize::MAX);
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        pb.tape.copy_within(
            self.input_offset..(self.input_offset + self.num_inputs),
            self.output_offset,
        );
    }
}
