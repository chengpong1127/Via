import argparse
import io
import pathlib
import subprocess
import tarfile
import tempfile

repo = pathlib.Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser()
parser.add_argument("--directory", type=pathlib.Path)
args = parser.parse_args()
root = args.directory or pathlib.Path(tempfile.mkdtemp(prefix="via-phase-b-reference-"))
root.mkdir(parents=True, exist_ok=True)
archive = subprocess.run(
    ["git", "-C", str(repo), "archive", "d7e20c4ed79b0e9891719d6bf0915d4007b14e2a", "server"],
    check=True,
    stdout=subprocess.PIPE,
).stdout
with tarfile.open(fileobj=io.BytesIO(archive)) as files:
    files.extractall(root, filter="data")

corridor = root / "server/src/routing/corridor.rs"
contents = corridor.read_text()
assert "    fn new(anchors:" in contents
corridor.write_text(contents.replace("    fn new(anchors:", "    pub(crate) fn new(anchors:"))
tests = root / "server/src/routing/evaluator/tests.rs"
tests.write_text(tests.read_text() + '\n#[path = "parallel_tests.rs"]\nmod parallel_tests;\n')
source = (repo / "server/src/routing/evaluator/parallel_tests.rs").read_text()
source = (
    source[:source.index("#[tokio::test]\nasync fn successful_evaluations")]
    + source[source.index("fn real_bike_session"):source.index("#[tokio::test]\nasync fn real_bike_cache")]
    + source[source.index("fn fingerprint"):source.index("#[tokio::test]\nasync fn mixed_and_bike")]
)
replacements = {
    "    query.limits.max_concurrent_phase2_states = concurrency;": "",
    "    concurrency: usize,\n    variant: &str,": "    _concurrency: usize,\n    variant: &str,",
    "    let mut query = query(&[TransitMode::Bus], 0, 1);": "    let query = query(&[TransitMode::Bus], 0, 1);",
    "    let matrix = Arc::new(crate::routing::matrix::SharedMatrixResolver::new(\n        provider.clone(),\n    ));":
        "    let matrix: Arc<dyn StreetRouter> = provider.clone();",
    "let walking = WalkingCache::with_resolver(matrix.clone())": "let mut walking = WalkingCache::new(matrix.clone())",
    "let bike = (variant": "let mut bike = (variant",
    "        &walking,\n        bike.as_ref(),": "        &mut walking,\n        bike.as_mut(),",
    "last.4.matrix.logical_requests": "(last.4.walking_matrix_requests + last.4.bike_matrix_requests)",
    "last.4.matrix.completed_hits": "0",
    "last.4.matrix.inflight_hits": "0",
    "last.4.matrix.misses": "last.1",
}
for original, replacement in replacements.items():
    assert original in source, f"fixture changed: {original!r}"
    source = source.replace(original, replacement)
(root / "server/src/routing/evaluator/parallel_tests.rs").write_text(source)
print(root / "server/Cargo.toml")
