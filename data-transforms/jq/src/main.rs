// Copyright 2024 Redpanda Data, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{anyhow, ensure, Context, Result};
use jaq_interpret::{Ctx, Filter, FilterT, ParseCtx, RcIter, Val};
use redpanda_transform_sdk::{on_record_written, BorrowedRecord, RecordWriter, WriteEvent};


// Use the talc custom allocator for our Wasm binary, it's both faster and smaller than the default
// allocator that Rust uses for Wasm.
// See: https://github.com/SFBdragon/talc/blob/master/talc/README_WASM.md
//
// SAFETY: The runtime environment must be single-threaded WASM.
#[cfg(target_family = "wasm")]
#[global_allocator]
static ALLOCATOR: talc::TalckWasm = unsafe { talc::TalckWasm::new_global() };

// This allows one to use $KEY to reference the record's key as a string.
const KEY_VAR: &str = "KEY";

fn main() -> Result<()> {
    let mut defs = ParseCtx::new(vec![KEY_VAR.to_owned()]);
    defs.insert_natives(jaq_core::core());
    defs.insert_defs(jaq_std::std());
    assert!(defs.errs.is_empty()); // These are builtins it should always be valid.
    let filter = std::env::var("FILTER").context("environment variable FILTER is required")?;
    let (f, errs) = jaq_parse::parse(&filter, jaq_parse::main());
    // TODO: report parse errors more gracefully
    ensure!(errs.is_empty(), "filter {filter} is invalid");
    let f = defs.compile(f.unwrap());
    ensure!(defs.errs.is_empty(), "filter {filter} is invalid");
    // Register our function that applies the jaq filter.
    on_record_written(|event, writer| jaq_transform(&f, event, writer));
}

// A transform of JSON payloads using [jaq](https://github.com/01mf02/jaq)
fn jaq_transform(filter: &Filter, event: WriteEvent, writer: &mut RecordWriter) -> Result<()> {
    // Parse our JSON from the value of the record.
    let payload = event.record.value().context("missing json")?;
    let json_payload: serde_json::Value = serde_json::from_slice(payload)?;
    let inputs = RcIter::new(core::iter::empty());
    // Add the key as a variable that can be referenced.
    let key = event
        .record
        .key()
        .map(|k| Val::str(String::from_utf8_lossy(k).to_string()))
        .unwrap_or(Val::Null);
    let ctx = Ctx::new(vec![key], &inputs);
    // Run the filter and write each JSON object to the output topic.
    for output in filter.run((ctx, Val::from(json_payload))) {
        let value = output.map_err(|e| anyhow!("error: {e}"))?;
        let value: serde_json::Value = value.into();
        let value = serde_json::to_vec(&value)?;
        writer.write(BorrowedRecord::new(event.record.key(), Some(&value)))?;
    }
    Ok(())
}

