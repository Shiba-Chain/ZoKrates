extern crate assert_cli;
extern crate serde_json;

#[cfg(test)]
mod integration {

    use glob::glob;
    use serde_json::from_reader;
    use std::fs;
    use std::fs::File;
    use std::io::{BufReader, Read};
    use std::panic;
    use std::path::Path;
    use tempdir::TempDir;
    use zokrates_abi::{parse_strict, Encode};
    use zokrates_core::typed_absy::abi::Abi;

    macro_rules! map(
    {
        $($key:expr => $value:expr),+ } => {
            {
                let mut m = ::std::collections::HashMap::new();
                $(m.insert($key, $value);)+
                m
            }
        };
    );

    #[test]
    #[ignore]
    fn test_compile_and_witness_dir() {
        // install nodejs dependencies for the verification contract tester
        install_nodejs_deps();

        let dir = Path::new("./tests/code");
        assert!(dir.is_dir());
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().unwrap() == "witness" {
                let program_name =
                    Path::new(Path::new(path.file_stem().unwrap()).file_stem().unwrap());
                let prog = dir.join(program_name).with_extension("zok");
                let witness = dir.join(program_name).with_extension("expected.witness");
                let json_input = dir.join(program_name).with_extension("arguments.json");
                test_compile_and_witness(
                    program_name.to_str().unwrap(),
                    &prog,
                    &json_input,
                    &witness,
                );
            }
        }
    }

    fn install_nodejs_deps() {
        let out_dir = concat!(env!("OUT_DIR"), "/contract");

        assert_cli::Assert::command(&["npm", "install"])
            .current_dir(out_dir)
            .succeeds()
            .unwrap();
    }

    fn test_compile_and_witness(
        program_name: &str,
        program_path: &Path,
        inputs_path: &Path,
        expected_witness_path: &Path,
    ) {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();
        let test_case_path = tmp_base.join(program_name);
        let flattened_path = tmp_base.join(program_name).join("out");
        let abi_spec_path = tmp_base.join(program_name).join("abi.json");
        let witness_path = tmp_base.join(program_name).join("witness");
        let inline_witness_path = tmp_base.join(program_name).join("inline_witness");
        let proof_path = tmp_base.join(program_name).join("proof.json");
        let verification_key_path = tmp_base
            .join(program_name)
            .join("verification")
            .with_extension("key");
        let proving_key_path = tmp_base
            .join(program_name)
            .join("proving")
            .with_extension("key");
        let verification_contract_path = tmp_base
            .join(program_name)
            .join("verifier")
            .with_extension("sol");

        // create a tmp folder to store artifacts
        fs::create_dir(test_case_path).unwrap();

        let stdlib = std::fs::canonicalize("../zokrates_stdlib/stdlib").unwrap();

        // prepare compile arguments
        let compile = vec![
            "../target/release/zokrates",
            "compile",
            "-i",
            program_path.to_str().unwrap(),
            "--stdlib-path",
            stdlib.to_str().unwrap(),
            "-s",
            abi_spec_path.to_str().unwrap(),
            "-o",
            flattened_path.to_str().unwrap(),
        ];

        // compile
        assert_cli::Assert::command(&compile).succeeds().unwrap();

        // COMPUTE_WITNESS

        let compute = vec![
            "../target/release/zokrates",
            "compute-witness",
            "-i",
            flattened_path.to_str().unwrap(),
            "-s",
            abi_spec_path.to_str().unwrap(),
            "-o",
            witness_path.to_str().unwrap(),
            "--stdin",
            "--abi",
        ];

        // run witness-computation for ABI-encoded inputs through stdin
        let json_input_str = fs::read_to_string(inputs_path).unwrap();

        assert_cli::Assert::command(&compute)
            .stdin(&json_input_str)
            .succeeds()
            .unwrap();

        // run witness-computation for raw-encoded inputs (converted) with `-a <arguments>`

        // First we need to convert our test input into raw field elements. We need to ABI spec for that
        let file = File::open(&abi_spec_path)
            .map_err(|why| format!("Could not open {}: {}", flattened_path.display(), why))
            .unwrap();

        let mut reader = BufReader::new(file);

        let abi: Abi = from_reader(&mut reader)
            .map_err(|why| why.to_string())
            .unwrap();

        let signature = abi.signature();

        let inputs_abi: zokrates_abi::Inputs<zokrates_field::Bn128Field> =
            parse_strict(&json_input_str, signature.inputs)
                .map(zokrates_abi::Inputs::Abi)
                .map_err(|why| why.to_string())
                .unwrap();
        let inputs_raw: Vec<_> = inputs_abi
            .encode()
            .into_iter()
            .map(|v| v.to_string())
            .collect();

        let mut compute_inline = vec![
            "../target/release/zokrates",
            "compute-witness",
            "-i",
            flattened_path.to_str().unwrap(),
            "-o",
            inline_witness_path.to_str().unwrap(),
        ];

        if !inputs_raw.is_empty() {
            compute_inline.push("-a");

            for arg in &inputs_raw {
                compute_inline.push(arg);
            }
        }

        assert_cli::Assert::command(&compute_inline)
            .succeeds()
            .unwrap();

        // load the expected witness
        let mut expected_witness_file = File::open(&expected_witness_path).unwrap();
        let mut expected_witness = String::new();
        expected_witness_file
            .read_to_string(&mut expected_witness)
            .unwrap();

        // load the actual witness
        let mut witness_file = File::open(&witness_path).unwrap();
        let mut witness = String::new();
        witness_file.read_to_string(&mut witness).unwrap();

        // load the actual inline witness
        let mut inline_witness_file = File::open(&inline_witness_path).unwrap();
        let mut inline_witness = String::new();
        inline_witness_file
            .read_to_string(&mut inline_witness)
            .unwrap();

        assert_eq!(inline_witness, witness);

        for line in expected_witness.as_str().split('\n') {
            assert!(
                witness.contains(line),
                "Witness generation failed for {}\n\nLine \"{}\" not found in witness",
                program_path.to_str().unwrap(),
                line
            );
        }

        #[cfg(feature = "libsnark")]
        let backends = map! {
            "bellman" => vec!["g16"],
            "libsnark" => vec!["pghr13"],
            "ark" => vec!["gm17", "marlin"]
        };

        #[cfg(not(feature = "libsnark"))]
        let backends = map! {
            "bellman" => vec!["g16"],
            "ark" => vec!["gm17", "marlin"]
        };

        // GENERATE A UNIVERSAL SETUP
        assert_cli::Assert::command(&[
            "../target/release/zokrates",
            "universal-setup",
            "--size",
            "15",
            "--proving-scheme",
            "marlin",
        ])
        .succeeds()
        .unwrap();

        for (backend, schemes) in backends {
            for scheme in &schemes {
                // SETUP
                let setup = assert_cli::Assert::command(&[
                    "../target/release/zokrates",
                    "setup",
                    "-i",
                    flattened_path.to_str().unwrap(),
                    "-p",
                    proving_key_path.to_str().unwrap(),
                    "-v",
                    verification_key_path.to_str().unwrap(),
                    "--backend",
                    backend,
                    "--proving-scheme",
                    scheme,
                ])
                .succeeds()
                .stdout()
                .doesnt_contain("This program is too small to generate a setup with Marlin")
                .execute();

                if setup.is_ok() {
                    // GENERATE-PROOF
                    assert_cli::Assert::command(&[
                        "../target/release/zokrates",
                        "generate-proof",
                        "-i",
                        flattened_path.to_str().unwrap(),
                        "-w",
                        witness_path.to_str().unwrap(),
                        "-p",
                        proving_key_path.to_str().unwrap(),
                        "--backend",
                        backend,
                        "--proving-scheme",
                        scheme,
                        "-j",
                        proof_path.to_str().unwrap(),
                    ])
                    .succeeds()
                    .unwrap();

                    // CLI VERIFICATION
                    assert_cli::Assert::command(&[
                        "../target/release/zokrates",
                        "verify",
                        "--backend",
                        backend,
                        "--proving-scheme",
                        scheme,
                        "-j",
                        proof_path.to_str().unwrap(),
                        "-v",
                        verification_key_path.to_str().unwrap(),
                    ])
                    .succeeds()
                    .unwrap();

                    if scheme != &"marlin" {
                        // EXPORT-VERIFIER
                        assert_cli::Assert::command(&[
                            "../target/release/zokrates",
                            "export-verifier",
                            "-i",
                            verification_key_path.to_str().unwrap(),
                            "-o",
                            verification_contract_path.to_str().unwrap(),
                            "--proving-scheme",
                            scheme,
                        ])
                        .succeeds()
                        .unwrap();

                        // TEST VERIFIER
                        assert_cli::Assert::command(&[
                            "node",
                            "test.js",
                            verification_contract_path.to_str().unwrap(),
                            proof_path.to_str().unwrap(),
                            scheme,
                        ])
                        .current_dir(concat!(env!("OUT_DIR"), "/contract"))
                        .succeeds()
                        .unwrap();
                    }
                }
            }
        }
    }

    fn test_compile_and_smtlib2(
        program_name: &str,
        program_path: &Path,
        expected_smtlib2_path: &Path,
    ) {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();
        let test_case_path = tmp_base.join(program_name);
        let flattened_path = tmp_base.join(program_name).join("out");
        let smtlib2_path = tmp_base.join(program_name).join("out.smt2");

        // create a tmp folder to store artifacts
        fs::create_dir(test_case_path).unwrap();

        let stdlib = std::fs::canonicalize("../zokrates_stdlib/stdlib").unwrap();

        // prepare compile arguments
        let compile = vec![
            "../target/release/zokrates",
            "compile",
            "-i",
            program_path.to_str().unwrap(),
            "--stdlib-path",
            stdlib.to_str().unwrap(),
            "-o",
            flattened_path.to_str().unwrap(),
        ];

        // compile
        assert_cli::Assert::command(&compile).succeeds().unwrap();

        // prepare generate-smtlib2 arguments
        let gen = vec![
            "../target/release/zokrates",
            "generate-smtlib2",
            "-i",
            flattened_path.to_str().unwrap(),
            "-o",
            smtlib2_path.to_str().unwrap(),
        ];

        // generate-smtlib2
        assert_cli::Assert::command(&gen).succeeds().unwrap();

        // load the expected smtlib2
        let mut expected_smtlib2_file = File::open(&expected_smtlib2_path).unwrap();
        let mut expected_smtlib2 = String::new();
        expected_smtlib2_file
            .read_to_string(&mut expected_smtlib2)
            .unwrap();

        // load the actual smtlib2
        let mut smtlib2_file = File::open(&smtlib2_path).unwrap();
        let mut smtlib2 = String::new();
        smtlib2_file.read_to_string(&mut smtlib2).unwrap();

        assert_eq!(expected_smtlib2, smtlib2);
    }

    #[test]
    #[ignore]
    fn test_compile_and_smtlib2_dir() {
        let dir = Path::new("./tests/code");
        assert!(dir.is_dir());
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().unwrap() == "smt2" {
                let program_name = Path::new(path.file_stem().unwrap());
                let prog = dir.join(program_name).with_extension("zok");
                test_compile_and_smtlib2(program_name.to_str().unwrap(), &prog, &path);
            }
        }
    }

    #[test]
    #[ignore]
    fn test_rng_tutorial() {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();

        for g in glob("./examples/book/rng_tutorial/*").expect("Failed to read glob pattern") {
            let path = g.unwrap();
            std::fs::hard_link(path.clone(), tmp_base.join(path.file_name().unwrap())).unwrap();
        }

        let stdlib = std::fs::canonicalize("../zokrates_stdlib/stdlib").unwrap();
        let binary_path = env!("CARGO_BIN_EXE_zokrates");

        assert_cli::Assert::command(&["./test.sh", binary_path, stdlib.to_str().unwrap()])
            .current_dir(tmp_base)
            .succeeds()
            .unwrap();
    }

    #[test]
    #[ignore]
    fn test_sha256_tutorial() {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();

        for g in glob("./examples/book/sha256_tutorial/*").expect("Failed to read glob pattern") {
            let path = g.unwrap();
            std::fs::hard_link(path.clone(), tmp_base.join(path.file_name().unwrap())).unwrap();
        }

        let stdlib = std::fs::canonicalize("../zokrates_stdlib/stdlib").unwrap();
        let binary_path = env!("CARGO_BIN_EXE_zokrates");

        assert_cli::Assert::command(&["./test.sh", binary_path, stdlib.to_str().unwrap()])
            .current_dir(tmp_base)
            .succeeds()
            .unwrap();
    }

    #[test]
    #[ignore]
    fn test_mpc_tutorial() {
        let tmp_dir = TempDir::new(".tmp").unwrap();
        let tmp_base = tmp_dir.path();

        for g in glob("./examples/book/mpc_tutorial/**/*").expect("Failed to read glob pattern") {
            let path = g.unwrap();
            std::fs::hard_link(path.clone(), tmp_base.join(path.file_name().unwrap())).unwrap();
        }

        let stdlib = std::fs::canonicalize("../zokrates_stdlib/stdlib").unwrap();
        let binary_path = env!("CARGO_BIN_EXE_zokrates");

        assert_cli::Assert::command(&["./test.sh", binary_path, stdlib.to_str().unwrap()])
            .current_dir(tmp_base)
            .succeeds()
            .unwrap();
    }
}
