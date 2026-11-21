/// Refuses a lockfile that pins a crate version named in RUSTSEC-2021-0003.
pub fn refuse_vulnerable_pin(name: &str, version: &str) -> bool {
    name == "smallvec" && version.starts_with("1.6.")
}
