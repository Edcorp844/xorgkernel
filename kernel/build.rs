fn main() {
    println!("cargo:rerun-if-changed=src/cpu/exceptions.S");
    println!("cargo:rerun-if-changed=src/cpu/irq.S");
    println!("cargo:rerun-if-changed=src/boot/grub_header.S");

    cc::Build::new()
        .file("src/cpu/exceptions.S")
        .file("src/cpu/irq.S")
        .file("src/boot/grub_header.S")
        .flag("-m32")
        .compile("kernel_cpu");
}
