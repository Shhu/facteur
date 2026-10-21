//! whisper.cpp's C API (speech to text), sharing llama.cpp's ggml.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, clippy::all)]

// The ggml symbols whisper calls come from there.
use llama_cpp_sys_2 as _;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
