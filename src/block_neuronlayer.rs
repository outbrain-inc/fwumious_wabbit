use std::any::Any;
use std::io;
use merand48::*;
use core::arch::x86_64::*;
use std::error::Error;
use std::mem::{self, MaybeUninit};
use rand::distributions::{Normal, Distribution};


use crate::optimizer;
use crate::regressor;
use crate::model_instance;
use crate::feature_buffer;
use crate::port_buffer;
use crate::consts;
use crate::block_helpers;
use crate::graph;

use optimizer::OptimizerTrait;
use regressor::BlockTrait;
use block_helpers::{Weight, WeightAndOptimizerData};


const MAX_NUM_INPUTS:usize= 16000;


#[derive(PartialEq)]
pub enum NeuronType {
    WeightedSum,
    LimitedWeightedSum,
}

#[derive(PartialEq)]
pub enum InitType {
    Random,
    RandomFirstNeuron1,
    RandomFirstNeuron10,
    One,
}



pub struct BlockNeuronLayer<L:OptimizerTrait> {    
    pub num_inputs: usize,
    pub input_offset: usize,
    pub output_offset: usize,
    pub weights_len: u32, 
    pub weights: Vec<WeightAndOptimizerData<L>>,
    pub optimizer: L,
    pub neuron_type: NeuronType,
    pub num_neurons: usize,
    pub init_type: InitType,
    pub dropout: f32,
    pub dropout_1: f32,
    pub max_norm: f32,
}


pub fn new_without_weights(mi: &model_instance::ModelInstance, 
                            num_inputs: usize, 
                            ntype: NeuronType, 
                            num_neurons: usize,
                            init_type: InitType, 
                            dropout: f32,
                            max_norm: f32) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    match mi.optimizer {
        model_instance::Optimizer::AdagradLUT => new_without_weights_2::<optimizer::OptimizerAdagradLUT>(&mi, num_inputs, ntype, num_neurons, init_type, dropout, max_norm),
        model_instance::Optimizer::AdagradFlex => new_without_weights_2::<optimizer::OptimizerAdagradFlex>(&mi, num_inputs, ntype, num_neurons, init_type, dropout, max_norm),
        model_instance::Optimizer::SGD => new_without_weights_2::<optimizer::OptimizerSGD>(&mi, num_inputs, ntype, num_neurons, init_type, dropout, max_norm)
    }
}


fn new_without_weights_2<L:OptimizerTrait + 'static>(mi: &model_instance::ModelInstance, 
                                                    num_inputs: usize, 
                                                    ntype: NeuronType, 
                                                    num_neurons: usize,
                                                    init_type: InitType,
                                                    dropout: f32,
                                                    max_norm: f32,
                                                    ) -> Result<Box<dyn BlockTrait>, Box<dyn Error>> {
    assert!(num_neurons > 0);
    assert!((num_inputs as usize )< MAX_NUM_INPUTS);
    assert!(num_inputs != 0);


    let weights_len = ((num_inputs + 1) * num_neurons as usize) as u32; // +1 is for bias term

    let mut rg = BlockNeuronLayer::<L> {
        weights: Vec::new(),
        output_offset: usize::MAX,
        input_offset: usize::MAX,
        num_inputs: num_inputs,
        optimizer: L::new(),
        weights_len: weights_len,
        neuron_type: ntype,
        num_neurons: num_neurons,
        init_type: init_type,
        dropout: dropout,
        dropout_1: 1.0 - dropout,
        max_norm: max_norm,
    };
    rg.optimizer.init(mi.learning_rate, mi.power_t, mi.init_acc_gradient);
//    rg.optimizer.init(mi.ffm_learning_rate, mi.ffm_power_t, mi.ffm_init_acc_gradient);
    Ok(Box::new(rg))
}


pub fn new_neuronlayer_block(bg: &mut graph::BlockGraph, 
                            mi: &model_instance::ModelInstance, 
                            input: graph::BlockPtrOutput,
                            ntype: NeuronType, 
                            num_neurons: usize,
                            init_type: InitType, 
                            dropout: f32,
                            max_norm: f32,
                        ) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {
    match mi.optimizer {
        model_instance::Optimizer::AdagradLUT => new_neuronlayer_block2::<optimizer::OptimizerAdagradLUT>(bg, &mi, input, ntype, num_neurons, init_type, dropout, max_norm),
        model_instance::Optimizer::AdagradFlex => new_neuronlayer_block2::<optimizer::OptimizerAdagradFlex>(bg, &mi, input, ntype, num_neurons, init_type, dropout, max_norm),
        model_instance::Optimizer::SGD => new_neuronlayer_block2::<optimizer::OptimizerSGD>(bg, &mi, input, ntype, num_neurons, init_type, dropout, max_norm)
    }
}


pub fn new_neuronlayer_block2<L:OptimizerTrait + 'static>(
                        bg: &mut graph::BlockGraph, 
                        mi: &model_instance::ModelInstance,
                        input: graph::BlockPtrOutput,
                        ntype: NeuronType, 
                        num_neurons: usize,
                        init_type: InitType, 
                        dropout: f32,
                        max_norm: f32,
                        ) -> Result<graph::BlockPtrOutput, Box<dyn Error>> {    
    let num_inputs = bg.get_num_outputs(vec![&input]);
    let block = new_without_weights_2::<L>(&mi, 
                                            num_inputs,
                                            ntype,
                                            num_neurons,
                                            init_type,
                                            dropout,
                                            max_norm).unwrap();
    let mut block_outputs = bg.add_node(block, vec![input]);
    assert_eq!(block_outputs.len(), 1);
    Ok(block_outputs.pop().unwrap())
}




impl <L:OptimizerTrait + 'static> BlockTrait for BlockNeuronLayer<L>

 {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn allocate_and_init_weights(&mut self, mi: &model_instance::ModelInstance) {
        assert!(self.weights_len != 0, "allocate_and_init_weights(): Have you forgotten to call set_num_inputs()?");
        self.weights =vec![WeightAndOptimizerData::<L>{weight:1.0, optimizer_data: self.optimizer.initial_data()}; self.weights_len as usize];
        // now set bias terms to zero
        
        // first neuron is always set to 1.0  
        let normal = Normal::new(0.0, (2.0/self.num_inputs as f32).sqrt() as f64);

        for i in 0..self.num_neurons * self.num_inputs {
    //            self.weights[i as usize].weight = (2.0 * merand48(((i*i+i) as usize) as u64)-1.0) * (1.0/(self.num_inputs as f32)).sqrt();
            self.weights[i as usize].weight = normal.sample(&mut rand::thread_rng()) as f32;
        }
        
        match self.init_type {
            InitType::Random => {},
            InitType::RandomFirstNeuron1 => { for i in 0..self.num_inputs { self.weights[i as usize].weight = 1.0}},
            InitType::RandomFirstNeuron10 => { for i in 0..self.num_inputs { self.weights[i as usize].weight = 0.0}; self.weights[0].weight = 1.0;},
            InitType::One => { for i in 0..self.weights_len { self.weights[i as usize].weight = 1.0}},

        }
        
        
//      
        
        for i in 0..self.num_neurons {
            self.weights[(self.num_neurons * self.num_inputs + i) as usize].weight = 0.0
        }
        
    }

    fn get_num_output_tapes(&self) -> usize {1}   


    fn get_num_outputs(&self, output_id: graph::BlockOutput) -> usize {
        assert!(output_id.get_output_id() == 0);
        self.num_neurons
    }

    
    fn set_input_offset(&mut self, input: graph::BlockInput, offset: usize)  {
        assert!(input.get_input_id() == 0);
        self.input_offset = offset;
    }

    fn set_output_offset(&mut self, output: graph::BlockOutput, offset: usize)  {
        assert!(output.get_output_id() == 0);
        self.output_offset = offset;
    }




    #[inline(always)]
    fn forward_backward(&mut self, 
                        further_blocks: &mut [Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        update:bool) {
        debug_assert!(self.num_inputs > 0);
        debug_assert!(self.output_offset != usize::MAX);
        debug_assert!(self.input_offset != usize::MAX);
        
        unsafe {

//          println!("len: {}, num inputs: {}, input_tape_indeX: {}", len, self.num_inputs, self.input_tape_index);
            let frandseed = fb.example_number * fb.example_number;
            let bias_offset = self.num_inputs * self.num_neurons;
            let mut j_offset:u32 = 0;
            for j in 0..self.num_neurons {
                let mut wsum:f32 = 0.0;
                if self.dropout == 0.0 || merand48(j as u64 + frandseed) > self.dropout {
                    wsum = self.weights.get_unchecked((bias_offset + j) as usize).weight; // bias term
                    let input_tape = pb.tape.get_unchecked(self.input_offset..(self.input_offset + self.num_inputs as usize));
                    for i in 0..self.num_inputs {                                 
                        wsum += input_tape.get_unchecked(i as usize) * self.weights.get_unchecked(i + j_offset as usize).weight;
                    }
                }
                j_offset += self.num_inputs as u32;
                if !update {wsum *= self.dropout_1;} // fix for overexcitment if we are just predicting and not learning
                *pb.tape.get_unchecked_mut(self.output_offset + j as usize) = wsum;
            }
            let (next_regressor, further_blocks) = further_blocks.split_at_mut(1);
            next_regressor[0].forward_backward(further_blocks, fb, pb, update);

            if update {
            {
//                let general_gradient = pb.tapes[self.output_tape_index as usize].pop().unwrap();
            
                if self.neuron_type == NeuronType::WeightedSum {
                    //let mut myslice = &mut pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize..];
                    // first we need to initialize inputs to zero
                    // TODO - what to think about this buffer
                    let mut output_errors: [f32; MAX_NUM_INPUTS] = MaybeUninit::uninit().assume_init();
                    for i in 0..self.num_inputs as usize {
                        output_errors[i] = 0.0; 
                    }

                    let output_tape = pb.tape.get_unchecked(self.output_offset..(self.output_offset + self.num_neurons as usize));
                    let input_tape = pb.tape.get_unchecked(self.input_offset..(self.input_offset + self.num_inputs as usize));
                    
                    for j in 0..self.num_neurons as usize {
                        if self.dropout == 0.0 || merand48(j as u64 + frandseed) > self.dropout {

                            let general_gradient = output_tape.get_unchecked(j);
                            let j_offset = j * self.num_inputs as usize;
   //                         println!("General gradient: {}", general_gradient);
                            for i in 0..self.num_inputs as usize {
                                let feature_value = input_tape.get_unchecked(i);
  //                              println!("input tape index: {}, input tape start: {}, i: {}", self.input_tape_index, input_tape_start, i);
 //                               println!("Wieght: {}, feature value: {}", self.weights.get_unchecked_mut(i + j_offset).weight, feature_value);
                                let gradient = general_gradient * feature_value;
//                            println!("Final gradient: {}", gradient);
                                let update = self.optimizer.calculate_update(gradient, 
                                                                        &mut self.weights.get_unchecked_mut(i + j_offset).optimizer_data);
                                *output_errors.get_unchecked_mut(i)  += self.weights.get_unchecked(i + j_offset).weight * general_gradient;
                                self.weights.get_unchecked_mut(i + j_offset).weight -= update;
                            }
                            {
                                // Updating bias term:
                                let gradient = general_gradient * 1.0;
                                let update = self.optimizer.calculate_update(gradient, 
                                                                            &mut self.weights.get_unchecked_mut(((self.num_inputs* self.num_neurons) as usize + j) as usize).optimizer_data);
                                self.weights.get_unchecked_mut(((self.num_inputs * self.num_neurons) as usize + j) as usize).weight -= update;
                            }
                            
                            
                            if self.max_norm != 0.0 && fb.example_number % 10 == 0 {
                                let mut wsumsquared = 0.0;
                                for i in 0..self.num_inputs as usize {
                                    let w = self.weights.get_unchecked_mut(i + j_offset).weight;
                                    wsumsquared += w * w;
                                }
                                let norm = wsumsquared.sqrt();
                                if norm > self.max_norm {
                                    let scaling = self.max_norm / norm;
                                    for i in 0..self.num_inputs as usize {
                                        self.weights.get_unchecked_mut(i + j_offset).weight *= scaling;
                                    }
                                } 
                            }
                            
                        }
                     }
                     
                    for i in 0..self.num_inputs as usize {
                        *pb.tape.get_unchecked_mut(self.input_offset + i) = *output_errors.get_unchecked(i);
                    }


                
                } else if self.neuron_type == NeuronType::LimitedWeightedSum {
                }
/*                    // Here it is like WeightedSum, but weights are limited to the maximum
                    let mut myslice = &mut pb.tapes[self.input_tape_index as usize][len - self.num_inputs as usize..];
                    for i in 0..myslice.len() {
                        let w = self.weights.get_unchecked(i).weight;
                        let feature_value = myslice.get_unchecked(i);
                        let gradient = general_gradient * feature_value;
                        let update = self.optimizer.calculate_update(gradient, &mut self.weights.get_unchecked_mut(i).optimizer_data);
                        self.weights.get_unchecked_mut(i).weight -= update;
                        if self.weights.get_unchecked_mut(i).weight > 1.0 {
                            self.weights.get_unchecked_mut(i).weight = 1.0;
                        } else if self.weights.get_unchecked_mut(i).weight < -1.0 {
                            self.weights.get_unchecked_mut(i).weight = -1.0;
                        }
                        
                        *myslice.get_unchecked_mut(i) = w * general_gradient;    // put the gradient on the tape in place of the value
                     }
                    
                }*/

            }
            
            // The only exit point
            return
        }
            
        } // unsafe end
    }
    
    fn forward(&self, further_blocks: &[Box<dyn BlockTrait>], 
                        fb: &feature_buffer::FeatureBuffer, 
                        pb: &mut port_buffer::PortBuffer, 
                        ) {
        assert!(false, "Unimplemented");    
    }
    
    fn get_serialized_len(&self) -> usize {
        return self.weights_len as usize;
    }

    fn read_weights_from_buf(&mut self, input_bufreader: &mut dyn io::Read) -> Result<(), Box<dyn Error>> {
        block_helpers::read_weights_from_buf(&mut self.weights, input_bufreader)
    }

    fn write_weights_to_buf(&self, output_bufwriter: &mut dyn io::Write) -> Result<(), Box<dyn Error>> {
        block_helpers::write_weights_to_buf(&self.weights, output_bufwriter)
    }

    fn read_weights_from_buf_into_forward_only(&self, input_bufreader: &mut dyn io::Read, forward: &mut Box<dyn BlockTrait>) -> Result<(), Box<dyn Error>> {
        let mut forward = forward.as_any().downcast_mut::<BlockNeuronLayer<optimizer::OptimizerSGD>>().unwrap();
        block_helpers::read_weights_only_from_buf2::<L>(self.weights_len as usize, &mut forward.weights, input_bufreader)
    }

    /// Sets internal state of weights based on some completely object-dependent parameters
    fn testing_set_weights(&mut self, aa: i32, bb: i32, index: usize, w: &[f32]) -> Result<(), Box<dyn Error>> {
        self.weights[index].weight = w[0];
        self.weights[index].optimizer_data = self.optimizer.initial_data();
        Ok(())
    }
}










mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use crate::block_misc;
    use crate::model_instance::Optimizer;
    use crate::feature_buffer;
    use crate::feature_buffer::HashAndValueAndSeq;
    use crate::vwmap;
    use crate::graph::BlockGraph;
    use block_helpers::{slearn, spredict, slearn2, spredict2};

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
    fn test_simple() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();        
        mi.learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.optimizer = Optimizer::SGD;
        
        let mut bg = BlockGraph::new();
        let input_block = block_misc::new_const_block(&mut bg, vec![2.0]).unwrap();
        let neuron_block = new_neuronlayer_block(&mut bg, 
                                            &mi, 
                                            input_block,
                                            NeuronType::WeightedSum, 
                                            1,
                                            InitType::One,
                                            0.0, // dropout
                                            0.0, // max norm
                                            ).unwrap();
        let result_block = block_misc::new_result_block2(&mut bg, neuron_block, 1.0).unwrap();
        bg.schedule();
        bg.allocate_and_init_weights(&mi);
        
        let mut pb = bg.new_port_buffer();
        
        let fb = fb_vec();
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 2.0);
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 1.5);
    }

    #[test]
    fn test_two_neurons() {
        let mut mi = model_instance::ModelInstance::new_empty().unwrap();        
        mi.learning_rate = 0.1;
        mi.power_t = 0.0;
        mi.optimizer = Optimizer::SGD;
        
        
        let NUM_NEURONS = 2;
        let mut bg = BlockGraph::new();
        let input_block = block_misc::new_const_block(&mut bg, vec![2.0]).unwrap();
        let neuron_block = new_neuronlayer_block(&mut bg, 
                                            &mi, 
                                            input_block,
                                            NeuronType::WeightedSum, 
                                            NUM_NEURONS,
                                            InitType::One,
                                            0.0, // dropout
                                            0.0, // max norm
                                            ).unwrap();
        let result_block = block_misc::new_result_block2(&mut bg, neuron_block, 1.0).unwrap();
        bg.schedule();
        bg.allocate_and_init_weights(&mi);
        
        let mut pb = bg.new_port_buffer();
        
        let fb = fb_vec();
        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, true), 2.0);
        // what do we expect:
        // on tape 0 input of 2.0 will be replaced with the gradient of 2.0
        // on tape 1 input has been consumed by returning function
        // on tape 2 the output was consumed by slearn
        assert_eq!(pb.results.len(), NUM_NEURONS as usize);  
        assert_eq!(pb.results[0], 2.0); // since we are using identity loss function, only one was consumed by slearn
        assert_eq!(pb.results[1], 2.0); // since we are using identity loss function, only one was consumed by slearn

        assert_epsilon!(slearn2  (&mut bg, &fb, &mut pb, false), 1.5);
        

    }


}



