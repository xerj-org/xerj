fn handler() { let a = std::env::args().nth(1).unwrap(); run(a); }
fn run(a: String) { Command::new("sh").arg("-c").arg(a); }
