fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("greet") => greet(&args.get(2).map(|s| s.as_str()).unwrap_or("world")),
        Some("add") => add(args.get(2), args.get(3)),
        Some("stats") => stats(),
        Some("help") | None => print_help(&args[0]),
        _ => eprintln!("unknown command. run {} help", args[0]),
    }
}

fn greet(name: &str) {
    println!("hello {name}");
}

fn add(a: Option<&String>, b: Option<&String>) {
    let x: i32 = a.and_then(|v| v.parse().ok()).unwrap_or(0);
    let y: i32 = b.and_then(|v| v.parse().ok()).unwrap_or(0);
    println!("{} + {} = {}", x, y, x + y);
}

fn stats() {
    let sum: i32 = (1..=100).sum();
    let avg = sum as f64 / 100.0;
    let fib = fibonacci(20);
    println!("sum(1..100)={sum}, avg={avg}, fib(20)={fib}");
}

fn fibonacci(n: u32) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fibonacci(n - 1) + fibonacci(n - 2),
    }
}

fn print_help(cmd: &str) {
    println!("usage: {cmd} <command> [args]");
    println!("commands:");
    println!("  greet [name]    say hello");
    println!("  add <a> <b>     add two numbers");
    println!("  stats           compute stats");
}
