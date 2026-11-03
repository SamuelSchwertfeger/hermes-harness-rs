#![no_main]

use hermes_harness::multiagent::ollama_discovery::parse_ollama_tags;
use hermes_harness::multiagent::ollama_native::decode_chat_completion_for_fuzzing;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_ollama_tags(data);
    let _ = decode_chat_completion_for_fuzzing(data);
});
