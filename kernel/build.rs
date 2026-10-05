fn main() {
    println!("cargo:rerun-if-changed=src/cpu/exceptions.S");
    println!("cargo:rerun-if-changed=src/cpu/irq.S");
    println!("cargo:rerun-if-changed=src/entry.S");

    cc::Build::new()
        .file("src/cpu/exceptions.S")
        .file("src/cpu/irq.S")
        .file("src/entry.S")
        .flag("-m32")
        .compile("kernel_cpu");
}
