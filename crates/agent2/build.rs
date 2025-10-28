fn main() {
    // Rebuild if any Handlebars templates change.
    println!("cargo:rerun-if-changed=src/templates");
}
