use crate::model_description::{FmiModelDescription, ScalarVariable};
use libfmi::{
    fmi2Boolean, fmi2Byte, fmi2CallbackFunctions, fmi2Component, fmi2FMUstate, fmi2Integer,
    fmi2Real, fmi2Status, fmi2Type, fmi2ValueReference, Fmi2Dll,
};
use std::{
    borrow::Borrow,
    collections::HashMap,
    env,
    ffi::CString,
    fmt::Display,
    fs, io,
    iter::zip,
    ops::Deref,
    os,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use thiserror::Error;
use zip::result::ZipError;

/// A unpacked FMU with a parsed model description.
#[derive(Debug)]
pub struct Fmu {
    #[allow(dead_code)]
    /// Optional tempdir that's used to hold the unpacked FMU.
    temp_dir: Option<tempfile::TempDir>,
    /// The directory of the unpacked FMU files.
    unpacked_dir: PathBuf,
    /// Parsed model description XML.
    pub model_description: FmiModelDescription,
}

/// An instance of a loaded FMU dynamic library.
pub struct FmuLibrary {
    /// The loaded dll library.
    fmi: Fmi2Dll,
    /// The simulation type of the loaded dll.
    ///
    /// Note that FMI specifies different libraries for CoSimulation vs ModelExchange
    /// so we must keep track of which library we loaded from this FMU.
    simulation_type: fmi2Type,
    /// The unpacked FMU. The FmuLibrary needs to take ownership of it to keep
    /// the tempdir alive.
    pub fmu: Fmu,
    /// Generates unique instance names for starting new FMU instances.
    instance_name_factory: InstanceNameFactory,
}

/// A simulation "instance", ready to execute.
pub struct FmuInstance<C: Borrow<FmuLibrary>> {
    /// The loaded dll library.
    ///
    /// This is generic behind the [`Borrow`] trait so that the user can pass in
    /// a reference or different Cell types such as [`Arc`].
    ///
    /// The presence of this enforces that the [`FmuLibrary`] will outlive the
    /// [`FmuInstance`].
    pub lib: C,
    /// A pointer to the "instance" we created by calling [`fmi2Instantiate`].
    instance: *mut os::raw::c_void,
    #[allow(dead_code)]
    callbacks: Box<fmi2CallbackFunctions>,
}

/// Generates unique instance names for starting new FMU instances.
struct InstanceNameFactory {
    model_identifier: String,
    /// This gets incremented every time we start a new instance of a simulation
    /// on the dll. Instances must have unique names so we append this counter
    /// to the instance name.
    instance_counter: AtomicUsize,
}

impl Deref for FmuLibrary {
    type Target = Fmu;

    /// Borrow to the inner [`Fmu`] type.
    fn deref(&self) -> &Self::Target {
        &self.fmu
    }
}

impl InstanceNameFactory {
    fn new(model_identifier: String) -> Self {
        Self {
            model_identifier,
            instance_counter: AtomicUsize::new(0),
        }
    }

    fn next(&self) -> String {
        let instance_counter = self.instance_counter.fetch_add(1, Ordering::Relaxed);
        format!("{}_{}", self.model_identifier, instance_counter)
    }
}

impl Fmu {
    /// Unpack an FMU file to a tempdir and parse it's model description.
    pub fn unpack(fmu_path: impl Into<std::path::PathBuf>) -> Result<Self, FmuUnpackError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("fmi-runner")
            .tempdir()
            .map_err(FmuUnpackError::NoTempdir)?;

        let fmu = Self::unpack_to(fmu_path, temp_dir.path())?;

        Ok(Self {
            temp_dir: Some(temp_dir),
            unpacked_dir: fmu.unpacked_dir,
            model_description: fmu.model_description,
        })
    }

    /// Unpack an FMU file to a given target dir and parse it's model description.
    pub fn unpack_to(
        fmu_path: impl Into<std::path::PathBuf>,
        target_dir: impl Into<std::path::PathBuf>,
    ) -> Result<Self, FmuUnpackError> {
        let fmu_path = fs::canonicalize(fmu_path.into()).map_err(FmuUnpackError::InvalidFile)?;
        let target_dir = target_dir.into();

        let zipfile = std::fs::File::open(fmu_path).map_err(FmuUnpackError::InvalidFile)?;
        let mut archive = zip::ZipArchive::new(zipfile).map_err(|e| match e {
            ZipError::Io(e) => FmuUnpackError::InvalidFile(e),
            e => FmuUnpackError::InvalidArchive(e),
        })?;
        archive.extract(&target_dir).map_err(|e| match e {
            ZipError::Io(e) => FmuUnpackError::InvalidOutputDir(e),
            e => FmuUnpackError::InvalidArchive(e),
        })?;

        let model_description = FmiModelDescription::new(&target_dir.join("modelDescription.xml"))?;

        Ok(Self {
            temp_dir: None,
            unpacked_dir: target_dir,
            model_description,
        })
    }

    /// Load the FMU dynamic library.
    pub fn load(self, simulation_type: fmi2Type) -> Result<FmuLibrary, FmuLoadError> {
        let (os_type, lib_type) = match env::consts::OS {
            "macos" => ("darwin", "dylib"),
            "linux" => ("linux", "so"),
            "windows" => ("win", "dll"),
            _ => ("unknown", "so"),
        };

        let arch_type = match std::env::consts::ARCH {
            "x86" => "32",
            "x86_64" => "64",
            // "arm" => "32",
            "aarch64" => "64",
            _ => "unknown",
        };

        let model_identifier = match simulation_type {
            fmi2Type::fmi2ModelExchange => self
                .model_description
                .model_exchange
                .as_ref()
                .ok_or(FmuLoadError::NoModelExchangeModel)?
                .model_identifier
                .clone(),
            fmi2Type::fmi2CoSimulation => self
                .model_description
                .co_simulation
                .as_ref()
                .ok_or(FmuLoadError::NoCoSimulationModel)?
                .model_identifier
                .clone(),
        };

        // construct the library folder string
        let lib_str = os_type.to_owned() + arch_type;

        // construct the full library path
        let mut lib_path = self
            .unpacked_dir
            .join("binaries")
            .join(lib_str)
            .join(&model_identifier);
        lib_path.set_extension(lib_type);

        FmuLibrary::load(lib_path, simulation_type, self, model_identifier)
    }

    pub fn variables(&self) -> &HashMap<String, ScalarVariable> {
        &self.model_description.model_variables.scalar_variable
    }
}

impl FmuLibrary {
    fn load(
        lib_path: impl Into<std::path::PathBuf>,
        simulation_type: fmi2Type,
        fmu: Fmu,
        model_identifier: String,
    ) -> Result<Self, FmuLoadError> {
        let fmi = unsafe { Fmi2Dll::new(lib_path.into()) }?;

        Ok(Self {
            fmi,
            simulation_type,
            fmu,
            instance_name_factory: InstanceNameFactory::new(model_identifier),
        })
    }
}

unsafe impl<C: Borrow<FmuLibrary>> Send for FmuInstance<C> {}

impl<C: Borrow<FmuLibrary>> FmuInstance<C> {
    /// Call `fmi2Instantiate()` on the FMU library to start a new simulation instance.
    pub fn instantiate(lib: C, logging_on: bool) -> Result<Self, FmuError> {
        let fmu_guid = &lib.borrow().model_description.guid;

        let callbacks = Box::<fmi2CallbackFunctions>::new(fmi2CallbackFunctions {
            logger: Some(libfmi::logger::callback_logger_handler),
            allocateMemory: Some(libc::calloc),
            freeMemory: Some(libc::free),
            stepFinished: None,
            componentEnvironment: std::ptr::null_mut::<std::os::raw::c_void>(),
        });

        let fmu_guid = CString::new(fmu_guid.as_bytes()).expect("Error building fmu_guid CString");

        let resource_location = "file://".to_owned()
            + lib
                .borrow()
                .unpacked_dir
                .join("resources")
                .to_str()
                .unwrap();
        let resource_location =
            CString::new(resource_location).expect("Error building resource_location CString");

        let visible = false as fmi2Boolean;
        let logging_on = logging_on as fmi2Boolean;

        // Generate a unique instance name to support multiple simulations at once.
        let instance_name = CString::new(lib.borrow().instance_name_factory.next())
            .expect("Error building instance_name CString");

        let instance = unsafe {
            lib.borrow().fmi.fmi2Instantiate(
                instance_name.as_ptr(),
                lib.borrow().simulation_type,
                fmu_guid.as_ptr(),
                resource_location.as_ptr(),
                &*callbacks,
                visible,
                logging_on,
            )
        };

        if instance.is_null() {
            return Err(FmuError::FmuInstantiateFailed);
        }

        Ok(Self {
            lib,
            instance,
            callbacks,
        })
    }

    pub fn get_types_platform(&self) -> &str {
        let types_platform =
            unsafe { std::ffi::CStr::from_ptr(self.lib.borrow().fmi.fmi2GetTypesPlatform()) }
                .to_str()
                .unwrap();
        types_platform
    }

    pub fn set_debug_logging(
        &self,
        logging_on: bool,
        log_categories: &[&str],
    ) -> Result<(), FmuError> {
        let category_cstr = log_categories
            .iter()
            .map(|c| CString::new(*c).unwrap())
            .collect::<Vec<_>>();

        let category_ptrs: Vec<_> = category_cstr.iter().map(|c| c.as_ptr()).collect();

        Self::ok_or_err(unsafe {
            self.lib.borrow().fmi.fmi2SetDebugLogging(
                self.instance,
                logging_on as fmi2Boolean,
                category_ptrs.len(),
                category_ptrs.as_ptr(),
            )
        })
    }

    pub fn setup_experiment(
        &self,
        start_time: f64,
        stop_time: Option<f64>,
        tolerance: Option<f64>,
    ) -> Result<(), FmuError> {
        Self::ok_or_err(unsafe {
            self.lib.borrow().fmi.fmi2SetupExperiment(
                self.instance,
                tolerance.is_some() as fmi2Boolean,
                tolerance.unwrap_or(0.0),
                start_time,
                stop_time.is_some() as fmi2Boolean,
                stop_time.unwrap_or(0.0),
            )
        })
    }

    pub fn enter_initialization_mode(&self) -> Result<(), FmuError> {
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2EnterInitializationMode(self.instance)
        })
    }

    pub fn exit_initialization_mode(&self) -> Result<(), FmuError> {
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2ExitInitializationMode(self.instance)
        })
    }

    pub fn get_reals<'fmu>(
        &'fmu self,
        signals: &[&'fmu ScalarVariable],
    ) -> Result<HashMap<&ScalarVariable, fmi2Real>, FmuError> {
        self.get(signals, Fmi2Dll::fmi2GetReal)
    }

    pub fn get_integers<'fmu>(
        &'fmu self,
        signals: &[&'fmu ScalarVariable],
    ) -> Result<HashMap<&ScalarVariable, fmi2Integer>, FmuError> {
        self.get(signals, Fmi2Dll::fmi2GetInteger)
    }

    pub fn get_booleans<'fmu>(
        &'fmu self,
        signals: &[&'fmu ScalarVariable],
    ) -> Result<HashMap<&ScalarVariable, fmi2Integer>, FmuError> {
        self.get(signals, Fmi2Dll::fmi2GetBoolean)
    }

    pub fn set_reals(
        &self,
        value_map: &HashMap<&ScalarVariable, fmi2Real>,
    ) -> Result<(), FmuError> {
        self.set(value_map, Fmi2Dll::fmi2SetReal)
    }

    pub fn set_integers(
        &self,
        value_map: &HashMap<&ScalarVariable, fmi2Integer>,
    ) -> Result<(), FmuError> {
        self.set(value_map, Fmi2Dll::fmi2SetInteger)
    }

    pub fn set_booleans(
        &self,
        value_map: &HashMap<&ScalarVariable, fmi2Integer>,
    ) -> Result<(), FmuError> {
        self.set(value_map, Fmi2Dll::fmi2SetBoolean)
    }

    pub fn do_step(
        &self,
        current_communication_point: fmi2Real,
        communication_step_size: fmi2Real,
        no_set_fmustate_prior_to_current_point: bool,
    ) -> Result<(), FmuError> {
        Self::ok_or_err(unsafe {
            self.lib.borrow().fmi.fmi2DoStep(
                self.instance,
                current_communication_point,
                communication_step_size,
                no_set_fmustate_prior_to_current_point as fmi2Boolean,
            )
        })
    }

    pub fn serialized_fmu_state_size(&self, size: &mut usize) -> Result<(), FmuError> {
        let mut fmu2state: fmi2FMUstate = std::ptr::null_mut();
        let pfmu2state = std::ptr::addr_of_mut!(fmu2state);
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2GetFMUstate(self.instance, pfmu2state)
        })?;
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2SerializedFMUstateSize(self.instance, *pfmu2state, size)
        })?;
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2FreeFMUstate(self.instance, pfmu2state)
        })
    }

    pub fn serialize_fmu_state(
        &self,
        serialized_state: &mut [u8],
        size: usize,
    ) -> Result<(), FmuError> {
        let mut fmu2state: fmi2FMUstate = std::ptr::null_mut();
        let pfmu2state = std::ptr::addr_of_mut!(fmu2state);
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2GetFMUstate(self.instance, pfmu2state)
        })?;
        let raw_serialized_state: *mut fmi2Byte = serialized_state.as_mut_ptr() as *mut i8;
        Self::ok_or_err(unsafe {
            self.lib.borrow().fmi.fmi2SerializeFMUstate(
                self.instance,
                *pfmu2state,
                raw_serialized_state,
                size,
            )
        })?;
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2FreeFMUstate(self.instance, pfmu2state)
        })
    }

    pub fn deserialize_fmu_state(
        &self,
        serialized_state: &[u8],
        size: usize,
    ) -> Result<(), FmuError> {
        let mut fmu2state: fmi2FMUstate = std::ptr::null_mut();
        let pfmu2state = std::ptr::addr_of_mut!(fmu2state);
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2GetFMUstate(self.instance, pfmu2state)
        })?;
        let raw_serialized_state: *const fmi2Byte = serialized_state.as_ptr() as *const i8;
        Self::ok_or_err(unsafe {
            self.lib.borrow().fmi.fmi2DeSerializeFMUstate(
                self.instance,
                raw_serialized_state,
                size,
                pfmu2state,
            )
        })?;
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2SetFMUstate(self.instance, *pfmu2state)
        })?;
        Self::ok_or_err(unsafe {
            self.lib
                .borrow()
                .fmi
                .fmi2FreeFMUstate(self.instance, pfmu2state)
        })
    }

    fn get<'fmu, T>(
        &'fmu self,
        signals: &[&'fmu ScalarVariable],
        func: unsafe fn(
            &Fmi2Dll,
            fmi2Component,
            *const fmi2ValueReference,
            usize,
            *mut T,
        ) -> fmi2Status,
    ) -> Result<HashMap<&'fmu ScalarVariable, T>, FmuError> {
        let mut values = Vec::<T>::with_capacity(signals.len());
        match unsafe {
            values.set_len(signals.len());
            func(
                &self.lib.borrow().fmi,
                self.instance,
                signals
                    .iter()
                    .map(|s| s.value_reference)
                    .collect::<Vec<_>>()
                    .as_ptr(),
                signals.len(),
                values.as_mut_ptr(),
            )
        } {
            fmi2Status::fmi2OK => Ok(zip(signals.to_owned(), values).collect()),
            status => Err(FmuError::BadFunctionCall(status)),
        }
    }

    fn set<T: Copy>(
        &self,
        value_map: &HashMap<&ScalarVariable, T>,
        func: unsafe fn(
            &Fmi2Dll,
            fmi2Component,
            *const fmi2ValueReference,
            usize,
            *const T,
        ) -> fmi2Status,
    ) -> Result<(), FmuError> {
        let len = value_map.len();
        let mut vrs = Vec::<fmi2ValueReference>::with_capacity(len);
        let mut values = Vec::<T>::with_capacity(len);

        for (signal, value) in value_map.iter() {
            vrs.push(signal.value_reference);
            values.push(*value);
        }

        Self::ok_or_err(unsafe {
            func(
                &self.lib.borrow().fmi,
                self.instance,
                vrs.as_ptr(),
                len,
                values.as_ptr(),
            )
        })
    }

    fn ok_or_err(status: fmi2Status) -> Result<(), FmuError> {
        match status {
            fmi2Status::fmi2OK => Ok(()),
            status => Err(FmuError::BadFunctionCall(status)),
        }
    }
}

impl<C: Borrow<FmuLibrary>> Drop for FmuInstance<C> {
    fn drop(&mut self) {
        unsafe { self.lib.borrow().fmi.fmi2FreeInstance(self.instance) };
    }
}

pub fn outputs_to_string<T: Display>(outputs: &HashMap<&ScalarVariable, T>) -> String {
    let mut s = String::new();

    for (signal, value) in outputs.iter() {
        s.push_str(&format!("{}: {:.3} | ", signal.name, value));
    }

    s
}

#[derive(Error, Debug)]
pub enum FmuUnpackError {
    #[error("Failed to create tempdir")]
    NoTempdir(#[source] io::Error),
    #[error("Invalid FMU path")]
    InvalidFile(#[source] io::Error),
    #[error("Invalid FMU unzip output directory")]
    InvalidOutputDir(#[source] io::Error),
    #[error("Invalid FMU archive")]
    InvalidArchive(#[from] ZipError),
    #[error("Invalid FMU model description XML")]
    InvalidModelDescription(#[from] quick_xml::DeError),
}

#[derive(Error, Debug)]
pub enum FmuLoadError {
    #[error("FMU does not contain CoSimulation model")]
    NoCoSimulationModel,
    #[error("FMU does not contain ModelExchange model")]
    NoModelExchangeModel,
    #[error("Error loading FMU dynamic library")]
    DLOpen(#[from] libloading::Error),
}

#[derive(Error, Debug)]
pub enum FmuError {
    #[error("FMU bad function call: {0:?}")]
    BadFunctionCall(fmi2Status),
    // #[error("FMU load error: {0}")]
    // LoadError(#[from] FmuLoadError),
    #[error("fmi2Instantiate() call failed")]
    FmuInstantiateFailed,
}

// test module
#[cfg(test)]
mod tests {
    use super::*;

    fn print_err(err: impl std::error::Error) {
        eprintln!("Display:\n{}", err);
        eprintln!("Debug:\n{:?}", err);
    }

    #[test]
    fn test_invalid_file() {
        let res = Fmu::unpack("dasf:?-()");
        assert!(matches!(res, Err(FmuUnpackError::InvalidFile { .. })));
        print_err(res.unwrap_err());
    }

    #[test]
    fn test_invalid_output_dir() {
        let res = Fmu::unpack_to("./tests/fmu/free_fall.fmu", "/z.(),.dasda/dasd");
        assert!(matches!(res, Err(FmuUnpackError::InvalidOutputDir { .. })));
        print_err(res.unwrap_err());
    }
}
