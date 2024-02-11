use std::{collections::HashMap, path::Path};

use fmu_runner::*;
use rand::{rngs::StdRng, Rng, SeedableRng};

#[test]
fn test_bouncing_ball() {
    let fmu = Fmu::unpack(Path::new("./tests/fmu/bouncing_ball.fmu"))
        .unwrap()
        .load(fmi2Type::fmi2CoSimulation)
        .unwrap();

    let signals = fmu.variables();
    println!("signals: {:?}", signals);

    {
        let fmu_cs = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();

        fmu_cs
            .set_reals(&HashMap::from([(&signals["h_start"], 10.0)]))
            .unwrap();

        // Enter initialization
        fmu_cs.enter_initialization_mode().unwrap();

        // Retrieve modified start values
        let values = fmu_cs.get_reals(&[&signals["h_start"]]).unwrap();
        assert_eq!(values[&signals["h_start"]], 10.0);

        // Exit initialization mode
        fmu_cs.exit_initialization_mode().unwrap();

        fmu_cs.do_step(0.0, 1.0, true).unwrap();

        let outputs = fmu_cs.get_reals(&[&signals["h_m"]]).unwrap();
        println!("{}", outputs_to_string(&outputs));
    }
}

// Save a snapshot and restore it later
#[test]
fn test_bouncing_ball_with_snapshot() {
    let fmu = Fmu::unpack(Path::new("./tests/fmu/BouncingBall.fmu"))
        .unwrap()
        .load(fmi2Type::fmi2CoSimulation)
        .unwrap();

    let signals = fmu.variables();

    {
        let fmu_cs = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();

        // Test configuration remaining in the free fall part before first bounce
        const H0: f64 = 10.0;
        const STEP_SIZE: f64 = 0.5;
        let h1: f64 = H0 + solve_free_fall(STEP_SIZE);
        let h2: f64 = H0 + solve_free_fall(2.0 * STEP_SIZE);
        let h_var = signals.get("h").unwrap();
        let h_val = || {
            fmu_cs
                .get_reals(&[h_var])
                .map(|hm| *hm.get(&h_var).unwrap())
                .unwrap()
        };

        fmu_cs.set_reals(&HashMap::from([(h_var, H0)])).unwrap();

        // Enter initialization
        fmu_cs.enter_initialization_mode().unwrap();

        // Retrieve modified start values
        let h = h_val();
        assert_eq!(h, H0);

        // Exit initialization mode
        fmu_cs.exit_initialization_mode().unwrap();

        fmu_cs.do_step(0.0, STEP_SIZE, true).unwrap();

        // Check h1 value
        let h = h_val();
        assert!(about_right(h, h1));

        // Get snapshot
        // - get its size
        let mut size = 0usize;
        fmu_cs.serialized_fmu_state_size(&mut size).unwrap();
        // - allocate receiving buffer
        let mut state = vec![0u8; size];
        // - get the snapshot
        fmu_cs.serialize_fmu_state(&mut state, size).unwrap();

        // In real life you would normally save the snapshot in a file or in a database
        // but here we just keep it in memory for simplicity of the test.

        // Execute one step
        fmu_cs.do_step(STEP_SIZE, STEP_SIZE, true).unwrap();

        // Check h2 value
        let h = h_val();
        assert!(about_right(h, h2));

        // Restore snapshot
        fmu_cs.deserialize_fmu_state(&state, size).unwrap();

        // Check h1 value
        let h = h_val();
        assert!(about_right(h, h1));

        // Execute one step.
        // Note that no_set_fmustate_prior_to_current_point argument is false
        // (beware the double negative: this means that fmu state was just set)
        fmu_cs.do_step(STEP_SIZE, STEP_SIZE, false).unwrap();

        // Check h2 value
        let h = h_val();
        assert!(about_right(h, h2));
    }
}

// Same with complete re-initialization before restore
#[test]
fn test_bouncing_ball_with_snapshot_reinit() {
    // Test configuration remaining in the free fall part before first bounce
    const H0: f64 = 10.0;
    const STEP_SIZE: f64 = 0.5;
    let h1: f64 = H0 + solve_free_fall(STEP_SIZE);
    let h2: f64 = H0 + solve_free_fall(2.0 * STEP_SIZE);

    let mut size = 0usize;
    let mut state;

    {
        let fmu = Fmu::unpack(Path::new("./tests/fmu/BouncingBall.fmu"))
            .unwrap()
            .load(fmi2Type::fmi2CoSimulation)
            .unwrap();

        let signals = fmu.variables();

        let fmu_cs = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();

        let h_var = signals.get("h").unwrap();
        let h_val = || {
            fmu_cs
                .get_reals(&[h_var])
                .map(|hm| *hm.get(&h_var).unwrap())
                .unwrap()
        };
    
        fmu_cs.set_reals(&HashMap::from([(h_var, H0)])).unwrap();

        // Enter initialization
        fmu_cs.enter_initialization_mode().unwrap();

        // Retrieve modified start values
        let h = h_val();
        assert_eq!(h, H0);

        // Exit initialization mode
        fmu_cs.exit_initialization_mode().unwrap();

        fmu_cs.do_step(0.0, STEP_SIZE, true).unwrap();

        // Check h1 value
        let h = h_val();
        assert!(about_right(h, h1));

        // Get snapshot
        // - get its size
        fmu_cs.serialized_fmu_state_size(&mut size).unwrap();
        // - allocate receiving buffer
        state = vec![0u8; size];
        // - get the snapshot
        fmu_cs.serialize_fmu_state(&mut state, size).unwrap();

        // Execute one step
        fmu_cs.do_step(STEP_SIZE, STEP_SIZE, true).unwrap();

        // Check h2 value
        let h = h_val();
        assert!(about_right(h, h2));
    }

    // Reload the FMU from disk, re-initialize it and reload the snapshot
    {
        let fmu = Fmu::unpack(Path::new("./tests/fmu/BouncingBall.fmu"))
            .unwrap()
            .load(fmi2Type::fmi2CoSimulation)
            .unwrap();

        let signals = fmu.variables();

        let fmu_cs = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();
        fmu_cs.enter_initialization_mode().unwrap();
        fmu_cs.exit_initialization_mode().unwrap();

        let h_var = signals.get("h").unwrap();
        let h_val = || {
            fmu_cs
                .get_reals(&[h_var])
                .map(|hm| *hm.get(&h_var).unwrap())
                .unwrap()
        };

        // Restore snapshot
        fmu_cs.deserialize_fmu_state(&state, size).unwrap();

        // Check h1 value
        let h = h_val();
        assert!(about_right(h, h1));

        // Execute one step with no_set_fmustate_prior_to_current_point = false
        fmu_cs.do_step(STEP_SIZE, STEP_SIZE, false).unwrap();

        // Check h2 value
        let h = h_val();
        assert!(about_right(h, h2));
    }
}

#[test]
fn test_point_mass() {
    let fmu = Fmu::unpack(Path::new("./tests/fmu/point_mass_pendulum.fmu"))
        .unwrap()
        .load(fmi2Type::fmi2CoSimulation)
        .unwrap();

    let signals = fmu.variables();
    println!("signals: {:?}", signals);

    {
        let fmu_cs = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();

        fmu_cs
            .set_reals(&HashMap::from([
                (&signals["mass_kg"], 100.0),
                (&signals["length_m"], 10.0),
            ]))
            .unwrap();

        fmu_cs.enter_initialization_mode().unwrap();
        fmu_cs.exit_initialization_mode().unwrap();

        fmu_cs.do_step(0.0, 1.0, true).unwrap();
        let outputs = fmu_cs
            .get_reals(&[&signals["theta_rad"], &signals["mass_kg"]])
            .unwrap();
        println!("{}", outputs_to_string(&outputs));

        // Change mass between steps.
        fmu_cs
            .set_reals(&HashMap::from([(&signals["mass_kg"], 10.0)]))
            .unwrap();

        fmu_cs.do_step(1.0, 1.0, true).unwrap();
        let outputs = fmu_cs
            .get_reals(&[&signals["theta_rad"], &signals["mass_kg"]])
            .unwrap();
        println!("{}", outputs_to_string(&outputs));

        assert_eq!(outputs[&signals["mass_kg"]], 10.0);
    }
}

#[test]
fn test_two_instances() {
    let fmu = Fmu::unpack(Path::new("./tests/fmu/free_fall.fmu"))
        .unwrap()
        .load(fmi2Type::fmi2CoSimulation)
        .unwrap();

    let signals = fmu.variables();

    {
        let fmu_cs_0 = FmuInstance::instantiate(&fmu, true).unwrap();
        let fmu_cs_1 = FmuInstance::instantiate(&fmu, true).unwrap();

        fmu_cs_0.setup_experiment(0.0, None, None).unwrap();
        fmu_cs_1.setup_experiment(0.0, None, None).unwrap();

        fmu_cs_0.enter_initialization_mode().unwrap();
        fmu_cs_1.enter_initialization_mode().unwrap();
        fmu_cs_0.exit_initialization_mode().unwrap();
        fmu_cs_1.exit_initialization_mode().unwrap();

        fmu_cs_0.do_step(0.0, 1.0, true).unwrap();
        fmu_cs_1.do_step(0.0, 1.1, true).unwrap();

        let outputs_0 = fmu_cs_0.get_reals(&[&signals["y_m"]]).unwrap();
        let outputs_1 = fmu_cs_1.get_reals(&[&signals["y_m"]]).unwrap();

        println!("outputs_0: {}", outputs_to_string(&outputs_0));
        println!("outputs_1: {}", outputs_to_string(&outputs_1));

        assert!(outputs_0[&signals["y_m"]] > outputs_1[&signals["y_m"]]);

        assert!(about_right(
            outputs_0[&signals["y_m"]],
            solve_free_fall(1.0)
        ));
        assert!(about_right(
            outputs_1[&signals["y_m"]],
            solve_free_fall(1.1)
        ));
    }
}

#[test]
fn test_box() {
    let fmu = Box::new(
        Fmu::unpack(Path::new("./tests/fmu/free_fall.fmu"))
            .unwrap()
            .load(fmi2Type::fmi2CoSimulation)
            .unwrap(),
    );

    {
        let fmu_cs = FmuInstance::instantiate(fmu, true).unwrap();

        let signals = fmu_cs.lib.variables();

        fmu_cs.setup_experiment(0.0, None, None).unwrap();

        fmu_cs.enter_initialization_mode().unwrap();
        fmu_cs.exit_initialization_mode().unwrap();

        fmu_cs.do_step(0.0, 1.0, true).unwrap();

        let outputs = fmu_cs.get_reals(&[&signals["y_m"]]).unwrap();

        println!("outputs: {}", outputs_to_string(&outputs));

        assert!(about_right(outputs[&signals["y_m"]], solve_free_fall(1.0)));
    }
}

fn solve_free_fall(t: f64) -> f64 {
    const G: f64 = -9.806;
    G * t.powi(2) / 2.0
}

fn about_right(a: f64, b: f64) -> bool {
    const EPSILON: f64 = 0.001;

    if a == b {
        return true;
    }

    let diff = (a - b).abs();
    let norm = f64::min(a.abs() + b.abs(), std::f64::MAX);
    if diff < EPSILON * norm {
        return true;
    } else {
        eprintln!("{} ~!= {}", a, b);
        return false;
    }
}

/// Fires up 1000 threads, each with a new FmuInstance steps them all simultaneously
/// for random but deterministic amounts of time verifying that the output is correct.
///
/// This is a stress test of the FMU library and the `fmu-runner` for race conditions.
#[test]
fn test_parallel_instances() {
    const THREAD_COUNT: usize = 1000;

    let fmu = Arc::new(
        Fmu::unpack(Path::new("./tests/fmu/free_fall.fmu"))
            .unwrap()
            .load(fmi2Type::fmi2CoSimulation)
            .unwrap(),
    );

    use std::sync::{Arc, Barrier};
    use std::thread;
    let barrier = Arc::new(Barrier::new(THREAD_COUNT));

    let mut rng = StdRng::seed_from_u64(42);

    let mut threads = Vec::new();
    for _ in 0..THREAD_COUNT {
        let barrier = barrier.clone();
        let fmu = fmu.clone();
        // let signals = signals.clone();
        let step_size = rng.gen_range(0.01..10.0);
        let step_count = rng.gen_range(1..100);
        threads.push(thread::spawn(move || {
            barrier.wait();

            let fmu_cs = FmuInstance::instantiate(fmu, true).unwrap();
            let signals = fmu_cs.lib.variables();

            fmu_cs.setup_experiment(0.0, None, None).unwrap();
            fmu_cs.enter_initialization_mode().unwrap();
            fmu_cs.exit_initialization_mode().unwrap();

            let mut sim_time = 0.0;

            for _ in 0..step_count {
                fmu_cs.do_step(sim_time, step_size, true).unwrap();
                sim_time += step_size;
            }

            let outputs = fmu_cs.get_reals(&[&signals["y_m"]]).unwrap();

            assert!(about_right(
                outputs[&signals["y_m"]],
                solve_free_fall(sim_time)
            ));
        }));
    }

    for thread in threads {
        thread.join().unwrap();
    }
}
