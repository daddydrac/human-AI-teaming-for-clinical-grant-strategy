fn main() {
    println!("cargo:rerun-if-changed=../hpc/hpc_kernels.cpp");
    cc::Build::new()
        .cpp(true)
        .file("../hpc/hpc_kernels.cpp")
        .flag_if_supported("-O3")
        .flag_if_supported("-ffast-math")
        .flag_if_supported("-march=native")
        .flag_if_supported("-fopenmp")
        .compile("grant_hpc");
    println!("cargo:rustc-link-lib=gomp");
    println!("cargo:rustc-link-lib=openblas");
}
