// `sqlx::migrate!` embeds migrations at compile time, but Cargo doesn't know
// to rebuild when one is added or removed; a stale build silently runs the
// old list (a test for a new migration failed against a cached build).
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
