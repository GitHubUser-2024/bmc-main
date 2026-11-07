// Copyright (C) 2026  Jan Novák
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Guest imports for streamed music playback (`host_media_*`), backed by
//! the process-wide player in [`crate::runtime::media`].

use anyhow::Result;
use wasmi::{Caller, Extern, Linker};

use crate::host_api::HostState;
use crate::runtime::media::{self, Command, MAX_LOAD_BYTES};

use super::super::memory::read_bytes;

pub(super) fn register(linker: &mut Linker<HostState>) -> Result<()> {
    register_load(linker)?;
    register_command(linker)?;
    register_version(linker)?;
    register_status(linker)?;
    Ok(())
}

/// `host_media_load(json_ptr, json_len) -> u32` — replace the queue.
///
/// 0 = loaded, 1 = malformed JSON, 2 = invalid queue, 3 = too large.
fn register_load(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "host_media_load",
        |caller: Caller<'_, HostState>, ptr: u32, len: u32| -> u32 {
            if usize::try_from(len).map_or(true, |len| len > MAX_LOAD_BYTES) {
                return 3;
            }
            let Some(json) = read_bytes(&caller, ptr, len) else {
                return 1;
            };
            match media::player().load(&json) {
                Ok(()) => 0,
                Err(e) => e as u32,
            }
        },
    )?;
    Ok(())
}

/// `host_media_command(command, arg) -> u32` — 1 when applied, 0 when the
/// command is unknown or doesn't apply now (see [`Command::from_wire`]).
fn register_command(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "host_media_command",
        |_caller: Caller<'_, HostState>, command: u32, arg: u32| -> u32 {
            Command::from_wire(command, arg)
                .map_or(0, |command| u32::from(media::player().command(command)))
        },
    )?;
    Ok(())
}

/// `host_media_version() -> u64` — changes whenever the status changes,
/// except for the position, which moves on every read anyway.
fn register_version(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "host_media_version",
        |_caller: Caller<'_, HostState>| -> u64 { media::player().version() },
    )?;
    Ok(())
}

/// `host_media_status(out_ptr, out_cap) -> u32` — the status as JSON,
/// probe-then-allocate like `host_system_snapshot`: returns the byte length
/// and writes only when `out_cap` is large enough.
fn register_status(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "host_media_status",
        |mut caller: Caller<'_, HostState>,
         out_ptr: u32,
         out_cap: u32|
         -> Result<u32, wasmi::Error> {
            let json = media::player().status_json();
            let needed = u32::try_from(json.len()).expect("BUG: status JSON fits u32");
            if out_cap < needed {
                return Ok(needed);
            }
            let Some(memory) = caller.get_export("memory").and_then(Extern::into_memory) else {
                return Err(wasmi::Error::new(
                    "host import `host_media_status`: guest module has no exported `memory`",
                ));
            };
            let data = memory.data_mut(&mut caller);
            let start = out_ptr as usize;
            let end = start.saturating_add(json.len());
            if end > data.len() {
                return Err(wasmi::Error::new(format!(
                    "host import `host_media_status`: out_ptr range {start:#x}..{end:#x} overflows \
                     guest memory of {} bytes",
                    data.len()
                )));
            }
            data[start..end].copy_from_slice(json.as_bytes());
            Ok(needed)
        },
    )?;
    Ok(())
}
