plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "com.modelmistress.runtime"
    compileSdk = 36
    ndkVersion = "27.2.12479018"

    defaultConfig {
        minSdk = 24
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// The Transferd overlay node (android/overlay-jni, crate mm-overlay-jni, which uses ../overlay) is built with cargo-ndk
// into this module's jniLibs. It needs Rust (cargo) with cargo-ndk and the aarch64-linux-android target. A build without
// them can skip it with -PskipOverlayNative=true; the app then reports the overlay as unavailable.
val overlayJniLibs = layout.buildDirectory.dir("generated/overlayJniLibs")
val skipOverlayNative = providers.gradleProperty("skipOverlayNative").map { it.toBoolean() }.orElse(false)

android.sourceSets.getByName("main").jniLibs.srcDir(overlayJniLibs)

val buildOverlayNative = tasks.register<Exec>("buildOverlayNative") {
    group = "build"
    description = "Builds the Transferd overlay JNI library (arm64-v8a) with cargo-ndk."
    onlyIf { !skipOverlayNative.get() }
    workingDir(rootProject.file("overlay-jni"))
    environment("ANDROID_NDK_HOME", android.ndkDirectory.absolutePath)
    commandLine(
        "cargo", "ndk", "-t", "arm64-v8a", "-o", overlayJniLibs.get().asFile.absolutePath,
        "build", "--release", "--locked",
    )
    inputs.dir(rootProject.file("overlay-jni/src"))
    inputs.dir(rootProject.file("../overlay/src"))
    inputs.files(rootProject.file("overlay-jni/Cargo.toml"), rootProject.file("overlay-jni/Cargo.lock"))
    inputs.files(rootProject.file("../overlay/Cargo.toml"), rootProject.file("../overlay/Cargo.lock"))
    outputs.dir(overlayJniLibs)
}

tasks.named("preBuild") {
    dependsOn(buildOverlayNative)
}

dependencies {
    api(project(":core"))
    api(project(":server"))
    implementation(project(":engine"))
    implementation(project(":litert"))
    implementation(project(":sd"))
    implementation(project(":cactus"))
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
}
