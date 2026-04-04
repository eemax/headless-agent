#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    use tempfile::TempDir;

    struct BenchHarness {
        _temp: TempDir,
        bin_dir: PathBuf,
        out_dir: PathBuf,
        home_dir: PathBuf,
    }

    impl BenchHarness {
        fn new() -> Self {
            let temp = TempDir::new().expect("tempdir");
            let bin_dir = temp.path().join("bin");
            let out_dir = temp.path().join("out");
            let home_dir = temp.path().join("home");
            fs::create_dir_all(&bin_dir).expect("create bin dir");
            fs::create_dir_all(&out_dir).expect("create out dir");
            fs::create_dir_all(&home_dir).expect("create home dir");

            let harness = Self {
                _temp: temp,
                bin_dir,
                out_dir,
                home_dir,
            };

            for utility in [
                "awk", "grep", "mkdir", "rm", "sed", "sort", "tail", "tr", "wc",
            ] {
                harness.link_system_command(utility);
            }
            harness.write_executable("node", "#!/bin/bash\nexit 0\n");

            harness
        }

        fn bin_path(&self, name: &str) -> PathBuf {
            self.bin_dir.join(name)
        }

        fn out_path(&self, name: &str) -> PathBuf {
            self.out_dir.join(name)
        }

        fn write_executable(&self, name: &str, body: &str) -> PathBuf {
            let path = self.bin_path(name);
            fs::write(&path, body).expect("write executable");
            let mut permissions = fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("chmod executable");
            path
        }

        fn link_system_command(&self, name: &str) {
            let resolved = resolve_system_command(name);
            symlink(&resolved, self.bin_path(name)).expect("symlink utility");
        }

        fn run(&self, filter: Option<&str>, headless: Option<&Path>) -> Output {
            let mut command = Command::new("/bin/bash");
            command.arg(script_path());
            if let Some(filter) = filter {
                command.arg(filter);
            }
            command.current_dir(env!("CARGO_MANIFEST_DIR"));
            command.env_clear();
            command.env("PATH", &self.bin_dir);
            command.env("HOME", &self.home_dir);
            command.env("BENCH_WEBFETCH_OUTDIR", &self.out_dir);
            if let Some(headless) = headless {
                command.env("HEADLESS", headless);
            }
            command.output().expect("run bench_webfetch.sh")
        }
    }

    #[test]
    fn filtered_run_passes_and_refreshes_output_dir() {
        let harness = BenchHarness::new();
        fs::write(harness.out_path("stale.txt"), "stale").expect("write stale file");

        let headless = harness.write_executable(
            "headless",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" != "webfetch" ]]; then
  echo "unexpected subcommand: ${1:-}" >&2
  exit 2
fi
if [[ "${2:-}" != "https://httpbin.org/html" ]]; then
  echo "unexpected url: ${2:-}" >&2
  exit 2
fi
printf '%s\n' \
  'URL: https://httpbin.org/html' \
  'Extraction: HtmlPrimary' \
  '---' \
  'Herman Melville appears in this long mock paragraph that comfortably exceeds forty characters and looks enough like extracted prose for the benchmark script.' \
  'Moby-Dick also appears in this second long paragraph, which helps push the body above the minimum length while still remaining unique for the duplication check.' \
  'A third long paragraph keeps the body large enough to clear the threshold and makes the mock output feel like realistic extracted content from a simple page.'
"#,
        );
        harness.write_executable(
            "defuddle",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" != "parse" || "${2:-}" != "--markdown" || "${3:-}" != "https://httpbin.org/html" ]]; then
  echo "unexpected args: $*" >&2
  exit 2
fi
printf '%s\n' \
  'A comparison output from defuddle that is shorter than headless but still substantial.' \
  'Second defuddle paragraph with enough characters to keep the size ratio healthy.'
"#,
        );
        harness.write_executable(
            "curl",
            r#"#!/bin/bash
set -euo pipefail
url="${!#}"
if [[ "$url" != "https://httpbin.org/html" ]]; then
  echo "unexpected url: $url" >&2
  exit 2
fi
printf '%s\n' '<html><body>Herman Melville and Moby-Dick</body></html>'
"#,
        );

        let output = harness.run(Some("httpbin_html"), Some(&headless));
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{}", combined_output(&output));
        assert!(stdout.contains("All checks passed."));
        assert!(stdout.contains("httpbin_html"));
        assert!(
            !harness.out_path("stale.txt").exists(),
            "stale output should be removed before each run"
        );

        let mut files = fs::read_dir(&harness.out_dir)
            .expect("read out dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .into_string()
                    .expect("utf8")
            })
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(
            files,
            vec![
                "httpbin_html_curl.txt",
                "httpbin_html_defuddle.txt",
                "httpbin_html_headless.txt",
            ]
        );
    }

    #[test]
    fn defuddle_failure_is_treated_as_a_skipped_size_comparison() {
        let harness = BenchHarness::new();

        let headless = harness.write_executable(
            "headless",
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' \
  'URL: https://httpbin.org/html' \
  'Extraction: HtmlPrimary' \
  '---' \
  'Herman Melville appears in this long mock paragraph that comfortably exceeds forty characters and looks enough like extracted prose for the benchmark script.' \
  'Moby-Dick also appears in this second long paragraph, which helps push the body above the minimum length while still remaining unique for the duplication check.' \
  'A third long paragraph keeps the body large enough to clear the threshold and makes the mock output feel like realistic extracted content from a simple page.'
"#,
        );
        harness.write_executable("defuddle", "#!/bin/bash\nexit 1\n");
        harness.write_executable(
            "curl",
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' '<html><body>Herman Melville and Moby-Dick</body></html>'
"#,
        );

        let output = harness.run(Some("httpbin_html"), Some(&headless));
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{}", combined_output(&output));
        assert!(stdout.contains("defuddle returned nothing — skipping size comparison"));
        assert!(stdout.contains("All checks passed."));
    }

    #[test]
    fn failing_run_aggregates_quality_issues() {
        let harness = BenchHarness::new();

        let headless = harness.write_executable(
            "headless",
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' \
  'URL: https://httpbin.org/html' \
  'Extraction: HtmlPrimary' \
  '---' \
  'This duplicated line is long enough for the duplication check and intentionally repeated for failure coverage.' \
  'This duplicated line is long enough for the duplication check and intentionally repeated for failure coverage.' \
  'This duplicated line is long enough for the duplication check and intentionally repeated for failure coverage.'
"#,
        );
        harness.write_executable(
            "defuddle",
            r#"#!/bin/bash
set -euo pipefail
printf '%0800d\n' 0
"#,
        );
        harness.write_executable(
            "curl",
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' '<html><body>fallback</body></html>'
"#,
        );

        let output = harness.run(Some("httpbin_html"), Some(&headless));
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(!output.status.success(), "{}", combined_output(&output));
        assert!(stdout.contains("headless is much smaller than defuddle"));
        assert!(stdout.contains("body content too small"));
        assert!(stdout.contains("possible content duplication"));
        assert!(stdout.contains("missing marker: \"Herman Melville\""));
        assert!(stdout.contains("missing marker: \"Moby-Dick\""));
        assert!(stdout.contains("5 failure(s):"));
    }

    #[test]
    fn missing_headless_binary_is_a_setup_error() {
        let harness = BenchHarness::new();

        let output = harness.run(
            Some("httpbin_html"),
            Some(&harness.bin_path("missing-headless")),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            combined_output(&output)
        );
        assert!(stdout.contains("error: headless binary not found"));
    }

    #[test]
    fn missing_defuddle_is_a_setup_error() {
        let harness = BenchHarness::new();
        let headless = harness.write_executable("headless", "#!/bin/bash\nexit 0\n");

        let output = harness.run(Some("httpbin_html"), Some(&headless));
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            combined_output(&output)
        );
        assert!(stdout.contains("error: defuddle not found"));
    }

    #[test]
    fn unknown_filter_lists_available_cases() {
        let harness = BenchHarness::new();
        let headless = harness.write_executable("headless", "#!/bin/bash\nexit 0\n");
        harness.write_executable("defuddle", "#!/bin/bash\nexit 0\n");

        let output = harness.run(Some("does_not_exist"), Some(&headless));
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            combined_output(&output)
        );
        assert!(stdout.contains("error: no case matched filter 'does_not_exist'"));
        assert!(stdout.contains("available: wikipedia_rust"));
        assert!(stdout.contains("httpbin_html"));
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("bench_webfetch.sh")
    }

    fn resolve_system_command(name: &str) -> PathBuf {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("could not resolve `{name}` on PATH"))
    }

    fn combined_output(output: &Output) -> String {
        format!(
            "stdout:\n{}\n\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}
