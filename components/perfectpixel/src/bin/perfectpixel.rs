fn main() {
    let output = perfectpixel::application::execute_cli(std::env::args().skip(1).collect());
    print!("{}", output.stdout);
    std::process::exit(output.exit_code);
}
