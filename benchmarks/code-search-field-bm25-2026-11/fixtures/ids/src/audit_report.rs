/// Audit report builder: lists which rustsec advisories affect which crates.
/// Report numbers look like 0003; years like 2021.
pub fn build_audit_report(year: u32, number: u32) -> String {
    format!("rustsec audit {year}-{number:04} affect summary")
}
