use std::any::Any;
use std::error::Error;
use std::io;

use crate::regressor;
use crate::feature_buffer;
use crate::port_buffer;
use crate::model_instance;
use crate::graph;

use regressor::BlockTrait;

pub struct BlockResult {
    num_inputs: u32,
    input_tape_index: i32,
    replace_input_with: f32,
}


pub fn new_result_block2(bg: &mut graph::BlockGraph,
                        input: graph::BlockPtrOutput,
                        replace_input_with: f32) 
                        -> Result<(), Box<dyn Error>> {

    let num_inputs = bg.get_num_outputs(vec![&input]);
    let block = Box::new(BlockResult {num_inputs: num_inputs,
                         input_tape_index: -1,
                         replace_input_with: replace_input_with});
    let block_outputs = bg.add_node(block, vec![input]);
    assert_eq!(block_outputs.len(), 0);
    Ok(())
}


pub fn new_result_block(num_inputs: u32, replace_input_with: f32) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    Ok(Box::new(BlockResult {num_inputs: num_inputs,
                             input_tape_index: -1,
                             replace_input_with: replace_input_with}))
}


impl BlockTrait for BlockResult {
    // Warning: It does not confirm to regular clean-up after itself

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn get_num_output_tapes(&self) -> usize {0}   

    fn get_num_outputs(&self) -> u32 {
        // this means outputs on regular tapes
        return 0
    }

    fn get_num_outputs2(&self, output_id: graph::BlockOutput) -> u32 {
        // this means outputs on regular tapes
        return 0
    }

    
    fn set_input_tape_index(&mut self, input_tape_index: i32) {
        self.input_tape_index = input_tape_index;
    }

    fn set_output_tape_index(&mut self, output_tape_index: i32) {
        panic!("Output tape of the BlockResult is automatically in result");
    }


    #[inline(always)]
    fn forward_backward(&mut self, 
                    further_blocks: &mut [Box<dyn BlockTrait>], 
                    fb: &feature_buffer::FeatureBuffer, 
                    pb: &mut port_buffer::PortBuffer, 
                    update:bool) {
        debug_assert!(self.input_tape_index >= 0);

        let len = pb.tapes[self.input_tape_index as usize].len();
        // Technically it needs to be longer. but for debugging we want to consume all of them
        if (self.num_inputs as usize) != len {
            panic!("BlockResult::forward_backward() Number of inputs is different than number of values on the input tape");
        }
        
        // copy inputs to result and replace inputs with whatever we have
        // copy inputs to result
        for x in 0..(self.num_inputs as usize) {
            let s = pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize + x];
            pb.results.push(s);
        }

        if further_blocks.len() > 0 {
            let (next_regressor, further_blocks) = further_blocks.split_at_mut(1);
            next_regressor[0].forward_backward(further_blocks, fb, pb, update);
        }
        
        // replace inputs with whatever we wanted
        for x in 0..(self.num_inputs as usize) {
            pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize + x] = self.replace_input_with;
        }

    }

    fn forward(&self, 
                     further_blocks: &[Box<dyn BlockTrait>], 
                     fb: &feature_buffer::FeatureBuffer,
                     pb: &mut port_buffer::PortBuffer, ) {
        debug_assert!(self.input_tape_index >= 0);

        let len = pb.tapes[self.input_tape_index as usize].len();
        // Technically it needs to be longer. but for debugging we want to consume all of them
        if (self.num_inputs as usize) != len {
            panic!("BlockSigmoid::forward_backward() Number of inputs is different than number of values on the input tape");
        }
        
        // copy inputs to result
        for x in 0..(self.num_inputs as usize) {
            let s = pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize + x];
            pb.results.push(s);
        }
        if further_blocks.len() > 0 {
            let (next_regressor, further_blocks) = further_blocks.split_at(1);
            next_regressor[0].forward(further_blocks, fb, pb);
        }

        // replace inputs with whatever we wanted
        for x in 0..(self.num_inputs as usize) {
            pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize + x] = self.replace_input_with;
        }


    }

}





pub struct BlockConsts {
    output_tape_index: i32,
    consts: Vec<f32>,
}
/*
pub fn new_const_block(consts: Vec<f32>) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    Ok(Box::new(BlockConsts {   output_tape_index: -1,
                                consts: consts}))
}*/

pub fn new_const_block( bg: &mut graph::BlockGraph, 
                        consts: Vec<f32>) 
                        -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let block = Box::new(BlockConsts {   output_tape_index: -1,
                                         consts: consts});
    let mut block_outputs = bg.add_node(block, vec![]);
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())


}



impl BlockTrait for BlockConsts {
    // Warning: It does not confirm to regular clean-up after itself

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn get_num_output_tapes(&self) -> usize {1}   

    fn get_num_outputs2(&self, output_id: graph::BlockOutput) -> u32 {
        assert_eq!(output_id.get_output_id(), 0); // Only one tape available
        self.consts.len() as u32
    }

    fn get_num_outputs(&self) -> u32 {
        self.consts.len() as u32
    }
    
    fn set_input_tape_index(&mut self, input_tape_index: i32) {
        panic!("You cannnot set input_tape_index for BlockConsts");

    }

    fn set_output_tape_index(&mut self, output_tape_index: i32) {
        self.output_tape_index = output_tape_index;
    }


    #[inline(always)]
    fn forward_backward(&mut self, 
                    further_blocks: &mut [Box<dyn BlockTrait>], 
                    fb: &feature_buffer::FeatureBuffer, 
                    pb: &mut port_buffer::PortBuffer, 
                    update:bool) {
        debug_assert!(self.output_tape_index >= 0);

        let output_original_size = pb.tapes[self.output_tape_index as usize].len();
        pb.tapes[self.output_tape_index as usize].extend_from_slice(&self.consts);

        if further_blocks.len() > 0 {
            let (next_regressor, further_blocks) = further_blocks.split_at_mut(1);
            next_regressor[0].forward_backward(further_blocks, fb, pb, update);
        }

        pb.tapes[self.output_tape_index as usize].truncate(output_original_size);
    }

    fn forward(&self, 
                     further_blocks: &[Box<dyn BlockTrait>], 
                     fb: &feature_buffer::FeatureBuffer,
                     pb: &mut port_buffer::PortBuffer, ) {
        debug_assert!(self.output_tape_index >= 0);

        let original_size = pb.tapes[self.output_tape_index as usize].len();
        pb.tapes[self.output_tape_index as usize].extend_from_slice(&self.consts);

        if further_blocks.len() > 0 {
            let (next_regressor, further_blocks) = further_blocks.split_at(1);
            next_regressor[0].forward(further_blocks, fb, pb);
        }

        pb.tapes[self.output_tape_index as usize].truncate(original_size );
    }

}


pub struct BlockCopy {    
    pub num_inputs: u32,
    pub input_tape_index: i32,
    pub output_tape_index: i32,
}


pub fn new_copy_block2(bg: &mut graph::BlockGraph,
                       input: graph::BlockPtrOutput
                       ) -> Result<Vec<graph::BlockPtrOutput>, Box<dyn Error>> {
    let num_inputs = bg.get_num_outputs(vec![&input]);
    assert!(num_inputs != 0);

    let mut block = Box::new(BlockCopy {
        output_tape_index: -1,
        input_tape_index: -1,
        num_inputs: num_inputs,
    });
    let block_outputs = bg.add_node(block, vec![input]);
    assert_eq!(block_outputs.len(), 2);
    Ok(block_outputs)
}

pub fn new_copy_block(mi: &model_instance::ModelInstance, 
                                                    num_inputs: u32, 
                                                    ) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    assert!(num_inputs != 0);
    let mut rg = BlockCopy {
        output_tape_index: -1,
        input_tape_index: -1,
        num_inputs: num_inputs,
    };
    Ok(Box::new(rg))
}






impl BlockTrait for BlockCopy

 {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {
    }

    fn get_num_output_tapes(&self) -> usize {2}   

    fn get_num_outputs2(&self, output_id: graph::BlockOutput) -> u32 {
        assert!(output_id.get_output_id() <= 1); // Copying to two tapes
        self.num_inputs
    }


    fn get_num_outputs(&self) -> u32 {
        return self.num_inputs
    }
    
    fn set_input_tape_index(&mut self, input_tape_index: i32) {
        self.input_tape_index = input_tape_index;
    }

    fn set_output_tape_index(&mut self, output_tape_index: i32) {
        self.output_tape_index = output_tape_index;
    }



    #[inline(always)]
    fn forward_backward(&mut self, 
                        further_blocks: &mut [Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        update:bool) {
        debug_assert!(self.output_tape_index >= 0);
        debug_assert!(self.input_tape_index >= 0);
        debug_assert!(self.input_tape_index != self.output_tape_index);
        debug_assert!(self.num_inputs > 0);
        
        unsafe {
            let len = pb.tapes[self.input_tape_index as usize].len();
            let output_tape_start = pb.tapes[self.output_tape_index as usize].len();
            let input_tape_start = pb.tapes[self.input_tape_index as usize].len() - self.num_inputs as usize; 
            let original_size = pb.tapes[self.output_tape_index as usize].len();
            pb.tapes[self.output_tape_index as usize].resize_with(original_size + (self.num_inputs) as usize, || {0.0});

            // plain copy from input to output
            for i in 0..self.num_inputs as usize {                                 
                let w = *(pb.tapes.get_unchecked(self.input_tape_index as usize)).get_unchecked(input_tape_start + i);
                *pb.tapes.get_unchecked_mut(self.output_tape_index as usize).get_unchecked_mut(output_tape_start +i) = w;
            }
                        
            //pb.tapes[self.output_tape_index as usize].extend_from_slice(pb.tapes.get_unchecked(self.input_tape_index as usize).get_unchecked(input_tape_start .. input_tape_start + self.num_inputs as usize));
            let (next_regressor, further_blocks) = further_blocks.split_at_mut(1);
            next_regressor[0].forward_backward(further_blocks, fb, pb, update);

            if update {
                // Sum up the gradients from output to input
                for i in 0..self.num_inputs as usize {
                    let w = *pb.tapes.get_unchecked(self.output_tape_index as usize).get_unchecked(output_tape_start +i);
//                    println!("AAAAAA: {}, initial: {}", w, *(pb.tapes.get_unchecked_mut(self.input_tape_index as usize)).get_unchecked_mut(input_tape_start + i));
                    *(pb.tapes.get_unchecked_mut(self.input_tape_index as usize)).get_unchecked_mut(input_tape_start + i) += w;
                }

            }
            pb.tapes[self.output_tape_index as usize].truncate(output_tape_start);
            
            // The only exit point
            return
            
        } // unsafe end
    }
    
    fn forward(&self, further_blocks: &[Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        ) {
        assert!(false, "Unimplemented");    
    }
    
}




pub struct BlockJoin {    
    pub num_inputs: u32,
    pub input_tape_index: i32,
    pub output_tape_index: i32,
}


pub fn new_join_block2(bg: &mut graph::BlockGraph,
                       inputs: Vec<graph::BlockPtrOutput>,
                       ) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    let num_inputs = bg.get_num_outputs(inputs.iter().collect());
    assert!(num_inputs != 0);

    let mut block = Box::new(BlockJoin {
        output_tape_index: -1,
        input_tape_index: -1,
        num_inputs: num_inputs,
    });
    let mut block_outputs = bg.add_node(block, inputs);
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}

impl BlockTrait for BlockJoin

 {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {
    }

    fn get_num_output_tapes(&self) -> usize {1}
    
    fn get_num_outputs2(&self, output_id: graph::BlockOutput) -> u32 {
        assert!(output_id.get_output_id() == 0); // Joining  two tapes
        self.num_inputs
    }

    fn get_num_outputs(&self) -> u32 {
        return self.num_inputs
    }
    
    fn set_input_tape_index(&mut self, input_tape_index: i32) {
        self.input_tape_index = input_tape_index;
    }

    fn set_output_tape_index(&mut self, output_tape_index: i32) {
        self.output_tape_index = output_tape_index;
    }



    #[inline(always)]
    fn forward_backward(&mut self, 
                        further_blocks: &mut [Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        update:bool) {
        debug_assert!(self.output_tape_index >= 0);
        debug_assert!(self.input_tape_index >= 0);
        debug_assert!(self.input_tape_index == self.output_tape_index);
        debug_assert!(self.num_inputs > 0);
        
        let (next_regressor, further_blocks) = further_blocks.split_at_mut(1);
        next_regressor[0].forward_backward(further_blocks, fb, pb, update);
    }
    
    fn forward(&self, further_blocks: &[Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        ) {
        assert!(false, "Unimplemented");    
    }
    
}













