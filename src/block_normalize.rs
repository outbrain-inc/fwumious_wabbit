use std::any::Any;
use std::error::Error;

use crate::block_helpers;
use crate::feature_buffer;
use crate::graph;
use crate::model_instance;
use crate::port_buffer;
use crate::regressor;
use regressor::BlockTrait;

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
    mi: &model_instance::ModelInstance,
    input: graph::BlockPtrOutput,
) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let num_inputs = bg.get_num_output_values(vec![&input]);
    assert!(num_inputs != 0);
    let mut block = Box::new(BlockNormalize {
        output_offset: usize::MAX,
        input_offset: usize::MAX,
        num_inputs: num_inputs,
    });
    let mut block_outputs = bg.add_node(block, vec![input])?;
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

impl BlockTrait for BlockNormalize {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {}

    fn get_num_output_slots(&self) -> usize {
        1
    }

    fn get_num_output_values(&self, output: graph::OutputSlot) -> usize {
        assert!(output.get_output_index() == 0);
        return self.num_inputs;
    }

    fn set_input_offset(&mut self, input: graph::InputSlot, offset: usize) {
        assert!(input.get_input_index() == 0);
        self.input_offset = offset;
    }

    fn set_output_offset(&mut self, output: graph::OutputSlot, offset: usize) {
        assert!(output.get_output_index() == 0);
        self.output_offset = offset;
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
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        unsafe {
            let mut mean: f32 = 0.0;
            for i in 0..self.num_inputs as usize {
                mean += *pb.tape.get_unchecked_mut(self.input_offset + i);
            }
            mean /= self.num_inputs as f32;
            let meansq = mean * mean;
            let mut variance: f32 = 0.0;
            for i in 0..self.num_inputs as usize {
                let w = meansq - *pb.tape.get_unchecked_mut(self.input_offset + i);
                variance += w * w;
            }
            variance += EPS;
            variance /= self.num_inputs as f32;
            variance = variance.sqrt();

            //            println!("var1: {}, var2: {}, sum: {}, sumsqr: {}", var1, var2, sum, sumsqr);
            let variance_inv = 1.0 / variance;
            //            let avg = sum / self.num_inputs as f32;

            for i in 0..self.num_inputs {
                *pb.tape.get_unchecked_mut(self.output_offset + i) =
                    (*pb.tape.get_unchecked(self.input_offset + i) - mean) * variance_inv;
            }
            block_helpers::forward_backward(further_blocks, fb, pb, update);

            if update {
                for i in 0..self.num_inputs {
                    *pb.tape.get_unchecked_mut(self.input_offset + i) =
                        *pb.tape.get_unchecked_mut(self.output_offset + i) * variance_inv;
                }
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
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        unsafe {
            /*            let mut sum: f32 = 0.0;
                        let mut sumsqr: f32 = 0.0;
                        let K = 1.0;
                        for i in 0..self.num_inputs as usize {
                                let w = *pb.tape.get_unchecked_mut(self.input_offset + i) - K;
                                sum += w;
                                sumsqr += w * w;
                        }
                        let var1 = (sumsqr - sum*sum/self.num_inputs as f32) /
                                        self.num_inputs as f32 + EPS;
                        let var2 = var1.sqrt();
            //            println!("var1: {}, var2: {}, sum: {}, sumsqr: {}", var1, var2, sum, sumsqr);
            */
            let mut mean: f32 = 0.0;
            for i in 0..self.num_inputs as usize {
                mean += *pb.tape.get_unchecked_mut(self.input_offset + i);
            }
            mean /= self.num_inputs as f32;
            let meansq = mean * mean;
            let mut variance: f32 = 0.0;
            for i in 0..self.num_inputs as usize {
                let w = meansq - *pb.tape.get_unchecked_mut(self.input_offset + i);
                variance += w * w;
            }
            variance += EPS;
            variance /= self.num_inputs as f32;
            variance = variance.sqrt();

            let var3 = 1.0 / variance;

            for i in 0..self.num_inputs {
                *pb.tape.get_unchecked_mut(self.output_offset + i) =
                    *pb.tape.get_unchecked(self.input_offset + i) * var3;
            }
            block_helpers::forward(further_blocks, fb, pb);
        } // unsafe end
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
    mi: &model_instance::ModelInstance,
    input: graph::BlockPtrOutput,
) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let num_inputs = bg.get_num_output_values(vec![&input]);
    debug_assert!(num_inputs != 0);
    let mut block = Box::new(BlockStopBackward {
        output_offset: usize::MAX,
        input_offset: usize::MAX,
        num_inputs: num_inputs,
    });
    let mut block_outputs = bg.add_node(block, vec![input])?;
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

impl BlockTrait for BlockStopBackward {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {}

    fn get_num_output_slots(&self) -> usize {
        1
    }

    fn get_num_output_values(&self, output: graph::OutputSlot) -> usize {
        assert!(output.get_output_index() == 0);
        return self.num_inputs;
    }

    fn set_input_offset(&mut self, input: graph::InputSlot, offset: usize) {
        assert!(input.get_input_index() == 0);
        self.input_offset = offset;
    }

    fn set_output_offset(&mut self, output: graph::OutputSlot, offset: usize) {
        assert!(output.get_output_index() == 0);
        self.output_offset = offset;
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
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);

        pb.tape.copy_within(
            self.input_offset..(self.input_offset + self.num_inputs),
            self.output_offset,
        );

        block_helpers::forward_backward(further_blocks, fb, pb, update);

        if update {
            pb.tape[self.input_offset..(self.input_offset + self.num_inputs)].fill(0.0);
        }
    }

    fn forward(
        &self,
        further_blocks: &[Box<dyn BlockTrait>],
        fb: &feature_buffer::FeatureBuffer,
        pb: &mut port_buffer::PortBuffer,
    ) {
        debug_assert!(self.output_offset != usize::MAX);
        debug_assert!(self.input_offset != usize::MAX);
        debug_assert!(self.num_inputs > 0);
        pb.tape.copy_within(
            self.input_offset..(self.input_offset + self.num_inputs),
            self.output_offset,
        );

        block_helpers::forward(further_blocks, fb, pb);
    }
}

/*
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use crate::block_misc;
    use crate::feature_buffer;
    use crate::feature_buffer::HashAndValueAndSeq;
    use crate::vwmap;
    use block_helpers::{slearn2, spredict2};
    use block_misc::{Observe};
    use crate::assert_epsilon;

    fn fb_vec() -> feature_buffer::FeatureBuffer {
        feature_buffer::FeatureBuffer {
                    label: 0.0,
                    example_importance: 1.0,
                    example_number: 0,
                    lr_buffer: Vec::new(),
                    ffm_buffer: Vec::new(),
                    ffm_fields_count: 0,
        }
    }


    #[test]
    fn test_simple_positive() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        let mut bg = BlockGraph::new();
        let input_block = block_misc::new_const_block(&mut bg, vec![2.0]).unwrap();
        let relu_block = new_relu_block(&mut bg, &mi, input_block).unwrap();
        let observe_block = block_misc::new_observe_block(&mut bg, relu_block, Observe::Forward, Some(1.0)).unwrap();
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        let fb = fb_vec();
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 2.0);
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 2.0); // relu desnt learn
    }
    fn test_simple_negative() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();
        let mut bg = BlockGraph::new();
        let input_block = block_misc::new_const_block(&mut bg, vec![-2.0]).unwrap();
        let relu_block = new_relu_block(&mut bg, &mi, input_block).unwrap();
        let observe_block = block_misc::new_observe_block(&mut bg, relu_block, Observe::Forward, Some(1.0)).unwrap();
        bg.finalize();
        bg.allocate_and_init_weights(&mi);

        let mut pb = bg.new_port_buffer();

        let fb = fb_vec();
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 0.0);
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 0.0); // relu desnt learn
    }


}



*/
