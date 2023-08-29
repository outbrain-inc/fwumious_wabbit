use crate::radix_tree::{NamespaceDescriptorWithHash, RadixTree};
use crate::vwmap;
use fasthash::murmur3;
use std::error::Error;
use std::fmt;
use std::io::BufRead;
use std::io::Error as IOError;
use std::io::ErrorKind;
use std::str;
use std::string::String;

const RECBUF_LEN: usize = 2048;
pub const HEADER_LEN: u32 = 3;
pub const NAMESPACE_DESC_LEN: u32 = 1;
pub const LABEL_OFFSET: usize = 1;
pub const EXAMPLE_IMPORTANCE_OFFSET: usize = 2;
pub const IS_NOT_SINGLE_MASK: u32 = 1u32 << 31;
pub const MASK31: u32 = !IS_NOT_SINGLE_MASK;
pub const NO_FEATURES: u32 = IS_NOT_SINGLE_MASK; // null is just an exact IS_NOT_SINGLE_MASK
pub const NO_LABEL: u32 = 0xff;
pub const FLOAT32_ONE: u32 = 1065353216; // 1.0f32.to_bits()

#[derive(Clone)]
pub struct VowpalParser {
    vw_map: vwmap::VwNamespaceMap,
    map_vwname_to_namespace_descriptor: RadixTree,
    tmp_read_buf: Vec<u8>,
    pub output_buffer: Vec<u32>,
}

#[derive(Debug)]
pub struct FlushCommand; // Parser returns FlushCommand to signal flush message
#[derive(Debug)]
pub struct HogwildLoadCommand {
    // Parser returns Hogwild Load as a command
    pub filename: String,
}

impl Error for FlushCommand {}
impl fmt::Display for FlushCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Not really an error: a \"flush\" command from client")
    }
}

impl Error for HogwildLoadCommand {}
impl fmt::Display for HogwildLoadCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Not really an error: a \"hogwild_load\" command from client to load: {}",
            self.filename
        )
    }
}

/*
organization of records buffer
(u32) length of the output record
(u32) label
(f32) Example importance (default: 1.0)
(union_u u32)[number of features], where:
    -- if the most significant bit is zero
            - this is a a binary namespace with a single feature
            - bits 1-31 are a feature hash
            - feature weight is implied to be 1.0 and is therefore not storred
    -- if the most significant bit is one
            - 14 next bits are the start offset, and lower 16 bits are the end offset of features beyond initial map
            - if this is a binary namespace the dynamic buffer content consists of the following pairs
                - the hash of the feature name (u32, bits 1-31), f32 weight of the feature)
            - if this is a f32 namespace the dynamic buffer content consists of the following pairs
                - the hash of the feature name (31 bits of u32), f32 parsed value of the feature name)
[dynamic buffer (of u32/f32 types, exact layout depends on the above bits)]
*/

impl VowpalParser {
    pub fn new(vw: &vwmap::VwNamespaceMap) -> VowpalParser {
        let mut map_vwname_to_namespace_descriptor = RadixTree::default();
        for (namespace_vwname_as_bytes, namespace_descriptor) in
            vw.map_vwname_to_namespace_descriptor.iter()
        {
            let namespace_hash_seed =
                murmur3::hash32(str::from_utf8(&namespace_vwname_as_bytes).unwrap());
            map_vwname_to_namespace_descriptor.insert(
                namespace_vwname_as_bytes,
                NamespaceDescriptorWithHash::new(namespace_descriptor.clone(), namespace_hash_seed),
            );
        }

        let mut parser = VowpalParser {
            vw_map: (*vw).clone(),
            map_vwname_to_namespace_descriptor,
            tmp_read_buf: Vec::with_capacity(RECBUF_LEN),
            output_buffer: Vec::with_capacity(RECBUF_LEN * 2),
        };
        parser.output_buffer.resize(
            (vw.num_namespaces as u32 * NAMESPACE_DESC_LEN + HEADER_LEN) as usize,
            0,
        );
        parser
    }

    pub fn print(&self) {
        log::info!("item out {:?}", self.output_buffer);
    }

    #[inline(always)]
    pub fn parse_float_or_error(
        &self,
        i_start: usize,
        i_end: usize,
        error_str: &str,
    ) -> Result<f32, Box<dyn Error>> {
        unsafe {
            if i_end - i_start == 4
                && *self.tmp_read_buf.get_unchecked(i_start) == b'N'
                && *self.tmp_read_buf.get_unchecked(i_start + 1) == b'O'
                && *self.tmp_read_buf.get_unchecked(i_start + 2) == b'N'
                && *self.tmp_read_buf.get_unchecked(i_start + 3) == b'E'
            {
                return Ok(f32::NAN);
            }

            match str::from_utf8_unchecked(self.tmp_read_buf.get_unchecked(i_start..i_end))
                .parse::<f32>()
            {
                Ok(f) => Ok(f),
                Err(_e) => Err(Box::new(IOError::new(
                    ErrorKind::Other,
                    format!(
                        "{}: {}",
                        error_str,
                        String::from_utf8_lossy(self.tmp_read_buf.get_unchecked(i_start..i_end))
                    ),
                ))),
            }
        }
    }

    // This is a very very slow implementation, but it's ok, this is called extremely infrequently to decode a command
    pub fn parse_cmd(&self, i_start: usize, rowlen: usize) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
        let mut o: Vec<Vec<u8>> = Vec::new();
        let mut i_end = i_start;
        while i_end < rowlen {
            let mut out_vec: Vec<u8> = Vec::new();
            while i_end < rowlen && self.tmp_read_buf[i_end] != 0x20 {
                out_vec.push(self.tmp_read_buf[i_end]);
                i_end += 1;
            }
            o.push(out_vec);
            while i_end < rowlen && self.tmp_read_buf[i_end] == 0x20 {
                i_end += 1;
            }
        }
        Ok(o)
    }

    pub fn next_vowpal(
        &mut self,
        input_bufread: &mut impl BufRead,
    ) -> Result<&[u32], Box<dyn Error>> {
        self.tmp_read_buf.truncate(0);
        let tmp_read_buf_size = match input_bufread.read_until(0x0a, &mut self.tmp_read_buf) {
            Ok(0) => return Ok(&[]),
            Ok(n) => n,
            Err(e) => Err(e)?,
        };
        return self.next_vowpal_to_size(tmp_read_buf_size);
    }

    pub fn next_vowpal_with_size(
        &mut self,
        input_bufread: &mut impl BufRead,
    ) -> Result<(&[u32], usize), Box<dyn Error>> {
        self.tmp_read_buf.truncate(0);
        let tmp_read_buf_size = match input_bufread.read_until(0x0a, &mut self.tmp_read_buf) {
            Ok(0) => return Ok((&[], 0)),
            Ok(n) => n,
            Err(e) => Err(e)?,
        };
        let size = if self
            .tmp_read_buf
            .last()
            .map_or(false, |value| *value == 0x0a)
        {
            tmp_read_buf_size - 1
        } else {
            tmp_read_buf_size
        };
        match self.next_vowpal_to_size(tmp_read_buf_size) {
            Ok(n) => Ok((n, size)),
            Err(e) => Err(e)?,
        }
    }

    pub fn next_vowpal_with_cache(
        &mut self,
        input_bufread: &mut impl BufRead,
        cached_tmp_read_buf_size: usize,
    ) -> Result<&[u32], Box<dyn Error>> {
        self.tmp_read_buf.truncate(cached_tmp_read_buf_size);

        let tmp_read_buf_size = match input_bufread.read_until(0x0a, &mut self.tmp_read_buf) {
            Ok(n) => n + cached_tmp_read_buf_size,
            Err(e) => Err(e)?,
        };

        if tmp_read_buf_size == 0 {
            return Ok(&[]);
        }
        return self.next_vowpal_to_size(tmp_read_buf_size);
    }

    fn next_vowpal_to_size(&mut self, tmp_read_buf_size: usize) -> Result<&[u32], Box<dyn Error>> {
        let bufpos: usize = self.vw_map.num_namespaces + HEADER_LEN as usize;

        let mut current_namespace_num_of_features = 0;

        unsafe {
            self.output_buffer.truncate(bufpos);
            self.output_buffer.fill(NO_FEATURES);

            let p = self.tmp_read_buf.as_ptr();
            let mut i_start: usize;
            let mut i_end: usize = 0;

            // first token is a label or "flush" command
            match *p.add(0) {
                0x31 => *self.output_buffer.get_unchecked_mut(LABEL_OFFSET) = 1, // 1
                0x2d => *self.output_buffer.get_unchecked_mut(LABEL_OFFSET) = 0, // -1
                0x7c => *self.output_buffer.get_unchecked_mut(LABEL_OFFSET) = NO_LABEL, // when first character is |, this means there is no label
                _ => {
                    // "flush" ascii 66, 6C, 75, 73, 68
                    if tmp_read_buf_size >= 5
                        && *p.add(0) == 0x66
                        && *p.add(1) == 0x6C
                        && *p.add(2) == 0x75
                        && *p.add(3) == 0x73
                        && *p.add(4) == 0x68
                    {
                        return Err(Box::new(FlushCommand));
                    } else if tmp_read_buf_size >= "hogwild_load ".len() {
                        // THIS IS SLOW, BUT IT IS CALLED VERY RARELY
                        // IF WE WILL AVE COMMANDS CALLED MORE FREQUENTLY, WE WILL NEED A FASTER IMPLEMENTATION
                        let vecs = self.parse_cmd(0, tmp_read_buf_size)?;
                        if vecs.len() == 2 {
                            let command = String::from_utf8_lossy(&vecs[0]);
                            if command == "hogwild_load" {
                                let filename = String::from_utf8_lossy(&vecs[1]);
                                return Err(Box::new(HogwildLoadCommand {
                                    filename: filename.to_string(),
                                }));
                            }
                        } else {
                            return Err(Box::new(IOError::new(
                                ErrorKind::Other,
                                "Cannot parse an example".to_string(),
                            )));
                        }
                    } else {
                        return Err(Box::new(IOError::new(
                            ErrorKind::Other,
                            "Cannot parse an example".to_string(),
                        )));
                        //                            return Err(Box::new(IOError::new(ErrorKind::Other, format!("Unknown first character of the label: ascii {:?}", *p.add(0)))))
                    }
                }
            };

            let rowlen = tmp_read_buf_size - 1; // ignore last newline byte
            if *self.output_buffer.get_unchecked(LABEL_OFFSET) == NO_LABEL {
                *self
                    .output_buffer
                    .get_unchecked_mut(EXAMPLE_IMPORTANCE_OFFSET) = FLOAT32_ONE;
            } else {
                // if we have a label, let's check if we also have label weight
                while *p.add(i_end) != 0x20 && i_end < rowlen {
                    i_end += 1;
                } // find space
                while *p.add(i_end) == 0x20 && i_end < rowlen {
                    i_end += 1;
                } // find first non-space
                  //if next character is not "|", we assume it's a example importance
                  //i_end +=1;
                if *p.add(i_end) == 0x7c {
                    *self
                        .output_buffer
                        .get_unchecked_mut(EXAMPLE_IMPORTANCE_OFFSET) = FLOAT32_ONE;
                } else {
                    // this token does not start with "|", so it has to be example importance floating point
                    i_start = i_end;
                    while *p.add(i_end) != 0x20 && i_end < rowlen {
                        i_end += 1;
                    } // find end of token (space)
                    let importance = self.parse_float_or_error(
                        i_start,
                        i_end,
                        "Failed parsing example importance",
                    )?;
                    if importance < 0.0 {
                        return Err(Box::new(IOError::new(
                            ErrorKind::Other,
                            format!("Example importance cannot be negative: {:?}! ", importance),
                        )));
                    }
                    *self
                        .output_buffer
                        .get_unchecked_mut(EXAMPLE_IMPORTANCE_OFFSET) = importance.to_bits();
                }
            }
            // Then we look for first namespace
            while *p.add(i_end) != 0x7c && i_end < rowlen {
                i_end += 1;
            }

            let mut current_namespace_hash_seed: u32 = 0;
            let mut current_namespace_index_offset: usize = HEADER_LEN as usize;
            let mut current_namespace_format = vwmap::NamespaceFormat::Categorical;

            let mut bufpos_namespace_start = 0;
            let mut current_namespace_weight: f32 = 1.0;
            while i_end < rowlen {
                // <letter>[:<weight>]

                // First skip spaces
                while *p.add(i_end) == 0x20 && i_end < rowlen {
                    i_end += 1;
                }
                i_start = i_end;
                while *p.add(i_end) != 0x20 && *p.add(i_end) != 0x3a && i_end < rowlen {
                    i_end += 1;
                } // 0x3a = ":"
                let i_end_first_part = i_end;
                while *p.add(i_end) != 0x20 && i_end < rowlen {
                    i_end += 1;
                }

                if *p.add(i_start) == 0x7c {
                    // "|"
                    // new namespace index
                    i_start += 1;
                    current_namespace_weight = if i_end_first_part != i_end {
                        // Non-empty part after ":" is namespace weight
                        self.parse_float_or_error(
                            i_end_first_part + 1,
                            i_end,
                            "Failed parsing namespace weight",
                        )?
                    } else {
                        1.0
                    };

                    let current_vwname = self.tmp_read_buf.get_unchecked(i_start..i_end_first_part);

                    let current_namespace_descriptor_with_hash =
                        match self.map_vwname_to_namespace_descriptor.get(current_vwname) {
                            Some(v) => v,
                            None => {
                                return Err(Box::new(IOError::new(
                                    ErrorKind::Other,
                                    format!(
                                    "Feature name was not predeclared in vw_namespace_map.csv: {}",
                                    String::from_utf8_lossy(
                                        &self.tmp_read_buf[i_start..i_end_first_part]
                                    )
                                ),
                                )))
                            }
                        };
                    let current_namespace_descriptor =
                        current_namespace_descriptor_with_hash.descriptor;
                    let current_namespace_index =
                        current_namespace_descriptor.namespace_index as usize;
                    current_namespace_hash_seed = current_namespace_descriptor_with_hash.hash_seed;
                    current_namespace_index_offset =
                        current_namespace_index * NAMESPACE_DESC_LEN as usize + HEADER_LEN as usize;
                    current_namespace_format = current_namespace_descriptor.namespace_format;
                    current_namespace_num_of_features = 0;
                    bufpos_namespace_start = self.output_buffer.len(); // this is only used if we will have multiple values
                } else {
                    // We have a feature! Let's hash it and write it to the buffer
                    let h = murmur3::hash32_with_seed(
                        self.tmp_read_buf.get_unchecked(i_start..i_end_first_part),
                        current_namespace_hash_seed,
                    ) & MASK31;

                    let feature_weight: f32 = if i_end_first_part != i_end {
                        // Non-empty part after ":" is namespace weight
                        self.parse_float_or_error(
                            i_end_first_part + 1,
                            i_end,
                            "Failed parsing feature weight",
                        )?
                    } else {
                        1.0
                    };

                    // We have three options:
                    // - first feature, no weights -> put it in-place
                    // - if it's second feature and first one was "simple", then promote it
                    // -- and then just add feature to the end of the buffer
                    if current_namespace_num_of_features == 0
                        && current_namespace_format == vwmap::NamespaceFormat::Categorical
                        && current_namespace_weight == 1.0
                        && feature_weight == 1.0
                    {
                        *self
                            .output_buffer
                            .get_unchecked_mut(current_namespace_index_offset) = h;
                    } else {
                        let feature_output = *self
                            .output_buffer
                            .get_unchecked(current_namespace_index_offset);
                        if (current_namespace_num_of_features == 1)
                            && (feature_output & IS_NOT_SINGLE_MASK) == 0
                        {
                            // We need to promote feature currently written in-place to out of place
                            self.output_buffer.push(feature_output);
                            self.output_buffer.push(FLOAT32_ONE);
                            debug_assert_eq!(
                                current_namespace_format,
                                vwmap::NamespaceFormat::Categorical
                            );
                        }
                        self.output_buffer.push(h);
                        if current_namespace_format == vwmap::NamespaceFormat::F32 {
                            // The namespace_skip_prefix allows us to parse a value A100, where A is one byte prefix which gets ignored
                            let float_start =
                                i_start + self.vw_map.vw_source.namespace_skip_prefix as usize;
                            let float_value: f32 = if i_end_first_part != float_start {
                                self.parse_float_or_error(
                                    float_start,
                                    i_end_first_part,
                                    "Failed parsing feature value to float (for float namespace)",
                                )?
                            } else {
                                f32::NAN
                            };
                            self.output_buffer.push(float_value.to_bits());
                            if current_namespace_weight * feature_weight != 1.0 {
                                return Err(Box::new(IOError::new(ErrorKind::Other, "Namespaces that are f32 can not have weight attached neither to namespace nor to a single feature (basically they can\' use :weight syntax".to_string())));
                            }
                        } else {
                            self.output_buffer
                                .push((current_namespace_weight * feature_weight).to_bits());
                        }
                        *self
                            .output_buffer
                            .get_unchecked_mut(current_namespace_index_offset) = IS_NOT_SINGLE_MASK
                            | (((bufpos_namespace_start << 16) + self.output_buffer.len()) as u32);
                    }
                    current_namespace_num_of_features += 1;
                }
                i_end += 1;
            }
        }

        //            println!("item out {:?} {}", self.output_buffer, bufpos);
        self.output_buffer[0] = self.output_buffer.len() as u32;
        Ok(&self.output_buffer)
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use std::io::Cursor;

    fn nd(start: u32, end: u32) -> u32 {
        (start << 16) + end
    }

    #[test]
    fn test_vowpal() {
        // Test for perfect vowpal-compatible hashing
        let vw_map_string = r#"
A,featureA
B,featureB
C,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();

        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let mut rr = VowpalParser::new(&vw);
        // we test a single record, single namespace
        let mut buf = str_to_cursor("1 |A a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // we test a single record, single namespace, space at the end
        let mut buf = str_to_cursor("1 |A a \n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // we test a single record, single namespace, space after label
        let mut buf = str_to_cursor("1  |A a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // we test a single record, single namespace, space between namespace and label
        let mut buf = str_to_cursor("1 |A  a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        let mut buf = str_to_cursor("-1 |B b\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                2422381320 & MASK31,
                NO_FEATURES
            ]
        );
        // single namespace with two features
        let mut buf = str_to_cursor("1 |A a b\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                10,
                1,
                FLOAT32_ONE,
                nd(6, 10) | IS_NOT_SINGLE_MASK, // |A
                NO_FEATURES,                    // |B
                NO_FEATURES,                    // |C
                2988156968 & MASK31,
                FLOAT32_ONE, // |A a
                3529656005 & MASK31,
                FLOAT32_ONE
            ]
        ); // |A b
           // two namespaces
        let mut buf = str_to_cursor("-1 |A a |B b\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                0,
                FLOAT32_ONE,
                2988156968 & MASK31,
                2422381320 & MASK31,
                NO_FEATURES
            ]
        );

        // two namespaces, double space
        let mut buf = str_to_cursor("-1 |A a  |B b\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                0,
                FLOAT32_ONE,
                2988156968 & MASK31,
                2422381320 & MASK31,
                NO_FEATURES
            ]
        );

        let mut buf = str_to_cursor("1 |UNDECLARED_NAMESPACE a\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(format!("{:?}", result), "Err(Custom { kind: Other, error: \"Feature name was not predeclared in vw_namespace_map.csv: UNDECLARED_NAMESPACE\" })");

        // namespace weight test
        let mut buf = str_to_cursor("1 |A:1.0 a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );
        // not a parsable number
        let mut buf = str_to_cursor("1 |A:not_a_parsable_number a\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(format!("{:?}", result), "Err(Custom { kind: Other, error: \"Failed parsing namespace weight: not_a_parsable_number\" })");

        // double weight
        let mut buf = str_to_cursor("1 |A:1:1 a\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Failed parsing namespace weight: 1:1\" })"
        );

        // namespace weight test
        let mut buf = str_to_cursor("1 |A:2.0 a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                1,
                FLOAT32_ONE,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                2988156968 & MASK31,
                2.0f32.to_bits()
            ]
        );
        // feature weight
        let mut buf = str_to_cursor("1 |A a:2.0\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                1,
                FLOAT32_ONE,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                2988156968 & MASK31,
                2.0f32.to_bits()
            ]
        );

        // two feature weights
        let mut buf = str_to_cursor("1 |A a:2.0 b:3.0\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                10,
                1,
                FLOAT32_ONE,
                nd(6, 10) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                2988156968 & MASK31,
                2.0f32.to_bits(),
                3529656005 & MASK31,
                3.0f32.to_bits(),
            ]
        );

        // feature weight + namespace weight
        let mut buf = str_to_cursor("1 |A:3 a:2.0\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                1,
                FLOAT32_ONE,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                2988156968 & MASK31,
                6.0f32.to_bits()
            ]
        );

        // bad feature weight
        let mut buf = str_to_cursor("1 |A a:2x0\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Failed parsing feature weight: 2x0\" })"
        );

        // first no weight, then two weighted features
        let mut buf = str_to_cursor("1 |A a b:2.0 c:3.0\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                12,
                1,
                FLOAT32_ONE,
                nd(6, 12) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                2988156968 & MASK31,
                1.0f32.to_bits(),
                3529656005 & MASK31,
                2.0f32.to_bits(),
                906509 & MASK31,
                3.0f32.to_bits(),
            ]
        );

        // LABEL TESTS
        // without label
        let mut buf = str_to_cursor("|A a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                NO_LABEL,
                FLOAT32_ONE,
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        /* Should we support this ?
        let mut buf = str_to_cursor(" |A a\n");
        assert_eq!(rr.next_vowpal(&mut buf).unwrap(), [6, NO_LABEL, FLOAT32_ONE,
                                                        2988156968 & MASK31,
                                                        NO_FEATURES,
                                                        NO_FEATURES]);
        */

        //println!("{:?}", rr.output_buffer);
        // now we test if end-of-stream works correctly
        str_to_cursor("");
        assert_eq!(rr.next_vowpal(&mut buf).unwrap().len(), 0);

        // flush should return [999]
        let mut buf = str_to_cursor("flush");
        assert!(rr.next_vowpal(&mut buf).err().unwrap().is::<FlushCommand>());

        // Unrecognized label -> Error
        let mut buf = str_to_cursor("$1");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Cannot parse an example\" })"
        );

        // Example importance is negative -> Error
        let mut buf = str_to_cursor("1 -0.1 |A a\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Example importance cannot be negative: -0.1! \" })"
        );

        // After label, there is neither namespace definition (|) nor example importance float
        let mut buf = str_to_cursor("1 fdsa |A a\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Failed parsing example importance: fdsa\" })"
        );

        // Example importance
        let mut buf = str_to_cursor("1 0.1 |A a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                0.1f32.to_bits(),
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // Example importance with bunch of spaces
        let mut buf = str_to_cursor("1  0.1  |A  a \n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                0.1f32.to_bits(),
                2988156968 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // flush should return FlushCommand
        let mut buf = str_to_cursor("flush");
        assert!(rr.next_vowpal(&mut buf).err().unwrap().is::<FlushCommand>());

        // flush should return FlushCommand
        let mut buf = str_to_cursor("hogwild_load /path/to/filename");
        let result = rr.next_vowpal(&mut buf).err().unwrap();
        assert!(result.is::<HogwildLoadCommand>());
        let hogwild_command = result.downcast_ref::<HogwildLoadCommand>().unwrap();
        assert_eq!(hogwild_command.filename, "/path/to/filename");

        // flush should return FlushCommand
        let mut buf = str_to_cursor("hogwild_load   /path/to/filename");
        let result = rr.next_vowpal(&mut buf).err().unwrap();
        assert!(result.is::<HogwildLoadCommand>());
        let hogwild_command = result.downcast_ref::<HogwildLoadCommand>().unwrap();
        assert_eq!(hogwild_command.filename, "/path/to/filename");

        // flush should return FlushCommand
        let mut buf = str_to_cursor("hogwild_load   /path/to/filename  ");
        let result = rr.next_vowpal(&mut buf).err().unwrap();
        assert!(result.is::<HogwildLoadCommand>());
        let hogwild_command = result.downcast_ref::<HogwildLoadCommand>().unwrap();
        assert_eq!(hogwild_command.filename, "/path/to/filename");

        // Check for two pathological cases - command without space, and command with a space but no file
        let mut buf = str_to_cursor("hogwild_load");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Cannot parse an example\" })"
        );

        let mut buf = str_to_cursor("hogwild_load ");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(
            format!("{:?}", result),
            "Err(Custom { kind: Other, error: \"Cannot parse an example\" })"
        );
    }

    #[test]
    fn test_float_namespaces() {
        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let vw_map_string = r#"
A,featureA
B,featureB
C,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();
        let mut rr = VowpalParser::new(&vw);
        // we test a single record, single namespace, with string value "3"
        let mut buf = str_to_cursor("-1 |B 3\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                1775699190 & MASK31,
                NO_FEATURES
            ]
        );

        let vw_map_string = r#"
A,featureA
B,featureB,f32
C,featureC
"#;

        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();
        let mut rr = VowpalParser::new(&vw);
        // we test a single record, single namespace, with string value "3"
        let mut buf = str_to_cursor("-1 |B 3\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                1775699190 & MASK31,
                3.0f32.to_bits()
            ]
        );

        let mut buf = str_to_cursor("-1 |B 3 4\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                10,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 10) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                1775699190 & MASK31,
                3.0f32.to_bits(),
                382082293 & MASK31,
                4.0f32.to_bits()
            ]
        );

        let mut buf = str_to_cursor("-1 |B not_a_number\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(format!("{:?}", result), "Err(Custom { kind: Other, error: \"Failed parsing feature value to float (for float namespace): not_a_number\" })");

        let mut buf = str_to_cursor("-1 |B 3 4\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                10,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 10) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                1775699190 & MASK31,
                3.0f32.to_bits(),
                382082293 & MASK31,
                4.0f32.to_bits()
            ]
        );

        let mut buf = str_to_cursor("-1 |B 3:3\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(format!("{:?}", result), "Err(Custom { kind: Other, error: \"Namespaces that are f32 can not have weight attached neither to namespace nor to a single feature (basically they can\' use :weight syntax\" })");

        let mut buf = str_to_cursor("-1 |B:3 3\n");
        let result = rr.next_vowpal(&mut buf);
        assert!(result.is_err());
        assert_eq!(format!("{:?}", result), "Err(Custom { kind: Other, error: \"Namespaces that are f32 can not have weight attached neither to namespace nor to a single feature (basically they can\' use :weight syntax\" })");

        // Now test with skip_prefix = 1
        let vw_map_string = r#"
A,featureA
B,featureB,f32
C,featureC
_namespace_skip_prefix,1
"#;

        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();
        let mut rr = VowpalParser::new(&vw);
        // we test a single record, single namespace, with string value "3"
        let mut buf = str_to_cursor("-1 |B B3\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                1416737454 & MASK31,
                3.0f32.to_bits()
            ]
        );

        // Because we skip one char, the float value of B is the float value of "" which is NAN
        let mut buf = str_to_cursor("-1 |B B\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                25602353 & MASK31,
                f32::NAN.to_bits()
            ]
        );

        let mut buf = str_to_cursor("-1 |B BNONE\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                0,
                FLOAT32_ONE,
                NO_FEATURES,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                1846432377 & MASK31,
                f32::NAN.to_bits()
            ]
        );
    }

    #[test]
    fn test_multibyte_namespaces() {
        // Test for perfect vowpal-compatible hashing
        let vw_map_string = r#"
AA,featureA
BB,featureB
CC,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();

        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let mut rr = VowpalParser::new(&vw);
        // we test a single record, single namespace
        let mut buf = str_to_cursor("1 |AA a\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                6,
                1,
                FLOAT32_ONE,
                292540976 & MASK31,
                NO_FEATURES,
                NO_FEATURES
            ]
        );

        // feature weight + namespace weight
        let mut buf = str_to_cursor("1 |AA:3 a:2.0\n");
        assert_eq!(
            rr.next_vowpal(&mut buf).unwrap(),
            [
                8,
                1,
                FLOAT32_ONE,
                nd(6, 8) | IS_NOT_SINGLE_MASK,
                NO_FEATURES,
                NO_FEATURES,
                292540976 & MASK31,
                6.0f32.to_bits()
            ]
        );
    }

    #[test]
    fn test_cache() {
        // Test for perfect vowpal-compatible hashing
        let vw_map_string = r#"
AA,featureA
BB,featureB
CC,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();

        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let mut rr = VowpalParser::new(&vw);

        let mut buf = str_to_cursor("|BB b |AA:3 a:2.0 \n");
        let buf_result = [
            8, 255, 1065353216, 2147876872, 1123906636, 2147483648, 292540976, 1086324736,
        ];

        assert_eq!(rr.next_vowpal(&mut buf).unwrap(), buf_result);

        let cache_input_str = "|BB b \n";
        let mut cache_buf = str_to_cursor(cache_input_str);
        let (cache_result, cache_result_size) = rr.next_vowpal_with_size(&mut cache_buf).unwrap();

        assert_eq!(
            cache_result,
            [6, 255, 1065353216, 2147483648, 1123906636, 2147483648]
        );

        let mut added_cache_buf = str_to_cursor("|AA:3 a:2.0 \n");

        // feature weight + namespace weight
        assert_eq!(
            rr.next_vowpal_with_cache(&mut added_cache_buf, cache_result_size)
                .unwrap(),
            buf_result
        );
    }

    #[test]
    fn test_cache_with_fully_cached_request() {
        // Test for perfect vowpal-compatible hashing
        let vw_map_string = r#"
AA,featureA
BB,featureB
CC,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();

        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let mut rr = VowpalParser::new(&vw);

        let mut buf = str_to_cursor("|BB b |AA:3 a:2.0 \n");
        let buf_result = [
            8, 255, 1065353216, 2147876872, 1123906636, 2147483648, 292540976, 1086324736,
        ];

        assert_eq!(rr.next_vowpal(&mut buf).unwrap(), buf_result);

        let cache_input_str = "|BB b |AA:3 a:2.0 \n";
        let mut cache_buf = str_to_cursor(cache_input_str);
        let (cache_result, cache_result_size) = rr.next_vowpal_with_size(&mut cache_buf).unwrap();

        assert_eq!(cache_result, buf_result);

        let mut added_cache_buf = str_to_cursor("");

        // feature weight + namespace weight
        assert_eq!(
            rr.next_vowpal_with_cache(&mut added_cache_buf, cache_result_size)
                .unwrap(),
            buf_result
        );
    }

    #[test]
    fn test_cache_with_empty_cached_request() {
        // Test for perfect vowpal-compatible hashing
        let vw_map_string = r#"
AA,featureA
BB,featureB
CC,featureC
"#;
        let vw = vwmap::VwNamespaceMap::new(vw_map_string).unwrap();

        fn str_to_cursor(s: &str) -> Cursor<Vec<u8>> {
            Cursor::new(s.as_bytes().to_vec())
        }

        let mut rr = VowpalParser::new(&vw);

        let mut buf = str_to_cursor("|BB b |AA:3 a:2.0 \n");
        let buf_result = [
            8, 255, 1065353216, 2147876872, 1123906636, 2147483648, 292540976, 1086324736,
        ];

        assert_eq!(rr.next_vowpal(&mut buf).unwrap(), buf_result);

        let cache_input_str = "";
        let mut cache_buf = str_to_cursor(cache_input_str);
        let (cache_result, cache_result_size) = rr.next_vowpal_with_size(&mut cache_buf).unwrap();

        let empty_result: &[u32] = &[];
        assert_eq!(cache_result, empty_result);

        let mut added_cache_buf = str_to_cursor("|BB b |AA:3 a:2.0 \n");

        // feature weight + namespace weight
        assert_eq!(
            rr.next_vowpal_with_cache(&mut added_cache_buf, cache_result_size)
                .unwrap(),
            buf_result
        );
    }
}
