//! A high level Rust wrapper for executing FMU's that follow the FMI 2.0 standard.
//!
//! This library contains bindings to the [fmi-standard](https://fmi-standard.org/)
//! and offers a high-level, safe API for unpacking, parsing, loading and executing FMU's.
//!
//! # Example
//!
//! ```
//! use std::{collections::HashMap, path::Path};
//! use fmu_runner::{Fmu, FmuInstance, fmi2Type};
//!
//! let fmu = Fmu::unpack(Path::new("./tests/fmu/bouncing_ball.fmu"))?
//!     .load(fmi2Type::fmi2CoSimulation)?;
//!
//! let fmu_cs = FmuInstance::instantiate(&fmu, true)?;
//! let signals = fmu_cs.lib.variables();
//!
//! fmu_cs.setup_experiment(0.0, None, None)?;
//!
//! // Set initial height to 10m.
//! fmu_cs.set_reals(&HashMap::from([(&signals["h_start"], 10.0)]))?;
//!
//! // Initialize model.
//! fmu_cs.enter_initialization_mode()?;
//! fmu_cs.exit_initialization_mode()?;
//!
//! // Step model 1 second.
//! fmu_cs.do_step(0.0, 1.0, true)?;
//!
//! // Get the current height.
//! let outputs = fmu_cs.get_reals(&[&signals["h_m"]])?;
//! println!("{}", fmu_runner::outputs_to_string(&outputs));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod fmu;
pub mod model_description;

pub use fmu::*;
pub use libfmi::fmi2Type;
