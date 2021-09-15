use crate::model_instance;
use crate::parser;
use crate::vwmap;
use std::error::Error;
use std::io::Error as IOError;
use std::io::ErrorKind;

use std::cell::RefCell;

use fasthash::murmur3;
use serde::{Serialize,Deserialize};
use dyn_clone::{clone_trait_object, DynClone};

use crate::feature_transform_parser;
use crate::feature_transform_parser::NamespaceTransforms;
use crate::feature_transform_implementations::{TransformerBinner, TransformerLogRatioBinner, TransformerCombine, TransformerWeight};



pub fn default_seeds(to_namespace_index: u32) -> [u32; 5] {
            [      
                murmur3::hash32_with_seed(vec![214, 231, 1, 55], to_namespace_index),
                murmur3::hash32_with_seed(vec![255, 6, 14, 69], to_namespace_index),
                murmur3::hash32_with_seed(vec![50, 6, 71, 123], to_namespace_index),
                murmur3::hash32_with_seed(vec![10, 3, 0,43] , to_namespace_index),
                murmur3::hash32_with_seed(vec![0, 53, 10, 201] , to_namespace_index),
            ]    
}

#[derive(Clone, Copy)]
pub enum SeedNumber {
    Default = 0,
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
}




#[derive(Clone)]
pub struct ExecutorToNamespace {
    pub namespace_index: u32,
    pub namespace_verbose: String,
    pub namespace_seeds: [u32; 5],	// These are precomputed namespace seeds
    pub tmp_data: Vec<(u32, f32)>,
}

#[derive(Clone)]
pub struct ExecutorFromNamespace {
    pub namespace_index: u32,
    pub namespace_verbose: String,	// This is actually not needed as we could just do a lookup each time
    pub namespace_is_float: bool,
}


impl ExecutorToNamespace {

    #[inline(always)]
    pub fn emit_i32(&mut self, to_data:i32, hash_value:f32, seed_id: SeedNumber) {
        let hash_index = murmur3::hash32_with_seed(to_data.to_le_bytes(), self.namespace_seeds[seed_id as usize]) & parser::MASK31;
        self.tmp_data.push((hash_index, hash_value));
    } 

    #[inline(always)]
    pub fn emit_f32(&mut self, f:f32, hash_value:f32, interpolated: bool, seed_id: SeedNumber) {
        if f.is_nan() {
            self.emit_i32(f as i32, hash_value, SeedNumber::Four);
        }
        else if interpolated {
            let floor = f.floor();
            let floor_int = floor as i32;
            let part = f - floor;
            if part != 0.0 {
                self.emit_i32(floor_int + 1, hash_value * part, seed_id);
            }
            let part = 1.0 - part;
            if part != 0.0 {
                self.emit_i32(floor_int, hash_value * part, seed_id);
            }
        } else {
            self.emit_i32(f as i32, hash_value, seed_id);
        }
    } 

    #[inline(always)]
    pub fn emit_i32_i32(&mut self, to_data1:i32, to_data2:i32, hash_value:f32, seed_id: SeedNumber) {
        let hash_index = murmur3::hash32_with_seed(to_data1.to_le_bytes(), self.namespace_seeds[seed_id as usize]);
        let hash_index = murmur3::hash32_with_seed(to_data2.to_le_bytes(), hash_index) & parser::MASK31;
        self.tmp_data.push((hash_index, hash_value));
    } 
}

#[derive(Clone)]
pub struct TransformExecutor {
    pub namespace_to: RefCell<ExecutorToNamespace>,
    function_executor: Box<dyn FunctionExecutorTrait>,
}

impl TransformExecutor {
    pub fn from_namespace_transform(namespace_transform: &feature_transform_parser::NamespaceTransform) -> Result<TransformExecutor, Box<dyn Error>> {
        
        let namespace_to = ExecutorToNamespace {
            namespace_index: namespace_transform.to_namespace.namespace_index,
            namespace_verbose: namespace_transform.to_namespace.namespace_verbose.to_owned(),
            // These are random numbers, i threw a dice!
            namespace_seeds: default_seeds(namespace_transform.to_namespace.namespace_index),
            tmp_data: Vec::new(),
        };
        
        let te = TransformExecutor {
            namespace_to: RefCell::new(namespace_to),
            function_executor: Self::create_executor(&namespace_transform.function_name, 
                                                    &namespace_transform.from_namespaces, 
                                                    &namespace_transform.function_parameters)?,
        };
        Ok(te)
    }

    pub fn create_executor(function_name: &str, namespaces_from: &Vec<feature_transform_parser::Namespace>, function_params: &Vec<f32>) -> Result<Box<dyn FunctionExecutorTrait>, Box<dyn Error>> {
        let mut executor_namespaces_from: Vec<ExecutorFromNamespace> = Vec::new();
        for namespace in namespaces_from {
            executor_namespaces_from.push(ExecutorFromNamespace{namespace_index: namespace.namespace_index, 
                                                                namespace_verbose: namespace.namespace_verbose.to_owned(),
                                                                namespace_is_float: namespace.namespace_is_float});
       }
        if        function_name == "BinnerSqrtPlain" {
            TransformerBinner::create_function(&(|x, resolution| x.sqrt() * resolution), function_name, &executor_namespaces_from, function_params, false)
        } else if function_name == "BinnerSqrt" {
            TransformerBinner::create_function(&(|x, resolution| x.sqrt() * resolution), function_name, &executor_namespaces_from, function_params, true)
        } else if function_name == "BinnerLogPlain" {
            TransformerBinner::create_function(&(|x, resolution| x.ln() * resolution), function_name, &executor_namespaces_from, function_params, false)
        } else if function_name == "BinnerLog" {
            TransformerBinner::create_function(&(|x, resolution| x.ln() * resolution), function_name, &executor_namespaces_from, function_params, true)
        } else if function_name == "BinnerLogRatioPlain" {
            TransformerLogRatioBinner::create_function(function_name, &executor_namespaces_from, function_params, false)
        } else if function_name == "BinnerLogRatio" {
            TransformerLogRatioBinner::create_function(function_name, &executor_namespaces_from, function_params, true)
        } else if function_name == "Combine" {
            TransformerCombine::create_function(function_name, &executor_namespaces_from, function_params)
        } else if function_name == "Weight" {
            TransformerWeight::create_function(function_name, &executor_namespaces_from, function_params)
        } else {
            return Err(Box::new(IOError::new(ErrorKind::Other, format!("Unknown transformer function: {}", function_name))));
        
        }
    }
}



#[derive(Clone)]
pub struct TransformExecutors {
    pub executors: Vec<TransformExecutor>,

}

impl TransformExecutors {
    pub fn from_namespace_transforms(namespace_transforms: &feature_transform_parser::NamespaceTransforms) -> TransformExecutors{
        let mut executors:Vec<TransformExecutor> = Vec::new();
        let mut namespaces_to: Vec<ExecutorToNamespace> = Vec::new();
        for transformed_namespace in &namespace_transforms.v {
            let transformed_namespace_executor = TransformExecutor::from_namespace_transform(&transformed_namespace).unwrap();
            executors.push(transformed_namespace_executor);

        }
        TransformExecutors {executors: executors}
    }

    #[inline(always)]
    pub fn get_transformations<'a>(&self, record_buffer: &[u32], feature_index_offset: u32) -> u32  {
        let executor_index = feature_index_offset & !feature_transform_parser::TRANSFORM_NAMESPACE_MARK; // remove transform namespace mark
        let executor = &self.executors[executor_index as usize];
        
        // If we have a cyclic defintion (which is a bug), this will panic!
        let mut namespace_to = executor.namespace_to.borrow_mut();
        namespace_to.tmp_data.truncate(0);
        
        executor.function_executor.execute_function(record_buffer, &mut namespace_to, &self);
        executor_index
    }


}


// Some black magic from: https://stackoverflow.com/questions/30353462/how-to-clone-a-struct-storing-a-boxed-trait-object
// We need clone() because of serving. There is also an option of doing FeatureBufferTransform from scratch in each thread
pub trait FunctionExecutorTrait: DynClone + Send {
    fn execute_function(&self, record_buffer: &[u32], to_namespace: &mut ExecutorToNamespace, transform_executors: &TransformExecutors);
}
clone_trait_object!(FunctionExecutorTrait);



mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use crate::parser::{IS_NOT_SINGLE_MASK, IS_FLOAT_NAMESPACE_MASK, MASK31};
    use crate::feature_transform_executor::default_seeds;

    #[test]
    fn test_interpolation() {
        let to_namespace_empty = ExecutorToNamespace {
                namespace_index: 1,
                namespace_verbose: "b".to_string(),
                namespace_seeds: default_seeds(1),	// These are precomputed namespace seeds
                tmp_data: Vec::new(),
            };
        let mut to_namespace = to_namespace_empty.clone();
        to_namespace.emit_f32(5.4, 20.0, true, SeedNumber::Default);
        let to_data_1:i32 = 6;
        let to_data_1_value = 20.0 * (5.4 - 5.0);
        let hash_index_1 = murmur3::hash32_with_seed(to_data_1.to_le_bytes(), to_namespace.namespace_seeds[SeedNumber::Default as usize]) & parser::MASK31;
        let to_data_2:i32 = 5;
        let to_data_2_value = 20.0 * (6.0 - 5.4);
        let hash_index_2 = murmur3::hash32_with_seed(to_data_2.to_le_bytes(), to_namespace.namespace_seeds[SeedNumber::Default as usize]) & parser::MASK31;
        assert_eq!(to_namespace.tmp_data, vec![(hash_index_1, to_data_1_value), (hash_index_2, to_data_2_value)]);            
    } 
}
