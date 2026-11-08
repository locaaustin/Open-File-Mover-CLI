//! Thin wrapper: all of the work lives in the library so that it can be
//! tested directly, without spawning processes.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(open_file_mover::main_with_args(std::env::args_os()))
}
