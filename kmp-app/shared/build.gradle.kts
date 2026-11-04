import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType
import org.jetbrains.kotlin.gradle.tasks.KotlinCompilationTask

plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.android.kotlin.multiplatform.library)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.compose.multiplatform)
}

kotlin {
    jvmToolchain(21)

    jvm("desktop")

    // Android library target via the KMP-native AGP plugin (DECISIONS D10).
    android {
        namespace = "org.theconstruct.shared"
        compileSdk = 36
        minSdk = 26
    }

    // Source set layout (DECISIONS D5):
    //   commonMain            models, renderer, ConstructApp, ConstructEngineBridge interface
    //   └─ jvmCommonMain      UniFFI bindings (uniffi.client_ffi) + UniffiEngineBridge (JNA)
    //      ├─ desktopMain     JNA jar, stdout logging
    //      └─ androidMain     JNA aar (bundles libjnidispatch.so), logcat logging
    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    applyDefaultHierarchyTemplate {
        common {
            group("jvmCommon") {
                withJvm()
                // withAndroidTarget() only matches the legacy androidTarget(); the KMP Android
                // library plugin's target is matched by platform type.
                withCompilations { it.target.platformType == KotlinPlatformType.androidJvm }
            }
        }
    }

    sourceSets {
        // BundledLayouts.kt (generateBundledLayouts below): each app's newest layout file, the offline fallback.
        commonMain { kotlin.srcDir(layout.buildDirectory.dir("generated/bundledLayouts/kotlin")) }
        commonMain.dependencies {
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(libs.compose.material3)
            implementation(compose.ui)
            // AsyncImage. `api` so the apps and the simulator can configure the ImageLoader.
            api(libs.coil.compose)
            api(libs.kotlinx.serialization.json)
            api(libs.kotlinx.coroutines.core)
        }
        commonTest.dependencies {
            implementation(kotlin("test"))
        }
        // JNA is added per platform, never in jvmCommonMain: desktop needs the plain jar, Android
        // the aar (the jar on Android would duplicate classes and lack the Android natives).
        // `api` because the generated bindings expose JNA types (Pointer) in public signatures.
        // Coil's OkHttp network fetcher (desktop JVM + Android), auto-registered via ServiceLoader.
        named("jvmCommonMain").dependencies {
            api(libs.coil.network.okhttp)
        }
        named("desktopMain").dependencies {
            api(libs.jna)
        }
        androidMain.dependencies {
            api("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
            // BackHandler (PlatformBackHandler's Android actual).
            implementation(libs.androidx.activity.compose)
        }
    }
}

// Builds the Rust cdylib (macOS) and regenerates the UniFFI Kotlin bindings
// (src/jvmCommonMain/kotlin/uniffi/client_ffi/client_ffi.kt, committed) before the desktop compile.
// Android compiles only need the committed bindings, so they don't depend on this task (they only
// order after it, in case both run in one build). Skip with -PskipRustBuild.
val repoRoot = rootDir.parentFile
val buildRustFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds client-ffi (cargo) and generates Kotlin bindings via scripts/build_ffi.sh"
    // Resolved at configuration time: Task.project at execution time breaks the configuration cache.
    val skipRustBuild = project.hasProperty("skipRustBuild")
    onlyIf { !skipRustBuild }
    workingDir = repoRoot
    commandLine("bash", File(repoRoot, "scripts/build_ffi.sh").absolutePath)
    // Gradle (esp. the daemon) may not inherit a login-shell PATH containing cargo.
    val cargoBin = "${System.getProperty("user.home")}/.cargo/bin"
    environment("PATH", "$cargoBin${File.pathSeparator}${System.getenv("PATH") ?: ""}")

    inputs.files(
        fileTree(File(repoRoot, "crates")) {
            include("**/src/**", "**/Cargo.toml", "**/build.rs")
            exclude("**/.*.swp")
        },
        File(repoRoot, "Cargo.toml"),
        File(repoRoot, "Cargo.lock"),
        File(repoRoot, "scripts/build_ffi.sh"),
    ).withPropertyName("rustSources")
    outputs.file(File(repoRoot, "target/release/libclient_ffi.dylib"))
    outputs.file(file("src/jvmCommonMain/kotlin/uniffi/client_ffi/client_ffi.kt"))
}

// The newest `layouts/<app>/NNNN.json` of every app, compiled into :shared as `BundledLayouts` (the layout a launcher
// draws before the pod has answered, docs/decisions/launcher-home-app.md). Generated from the files, never copied by
// hand. Each layout is a string split into chunks: one JVM string constant holds at most 65535 bytes.
val generateBundledLayouts by tasks.registering {
    group = "build"
    description = "Embeds each app's newest layout file as BundledLayouts (offline fallback)"
    val layoutsDir = File(repoRoot, "layouts")
    val outDir = layout.buildDirectory.dir("generated/bundledLayouts/kotlin")
    inputs.dir(layoutsDir).withPropertyName("layouts")
    outputs.dir(outDir)
    doLast {
        val apps = (layoutsDir.listFiles() ?: emptyArray()).filter { it.isDirectory }.sortedBy { it.name }
        val out = StringBuilder()
        out.append("// Generated by :shared:generateBundledLayouts from layouts/<app>/ (the newest file of each). Do not edit.\n")
        out.append("package org.theconstruct\n\n")
        out.append("internal object BundledLayoutFiles {\n")
        out.append("    /** App id -> (file number, the file's JSON). */\n")
        out.append("    val files: Map<String, Pair<Int, String>> = mapOf(\n")
        for (app in apps) {
            val newest = (app.listFiles() ?: emptyArray())
                .filter { Regex("""\d{4}\.json""").matches(it.name) }
                .maxByOrNull { it.name } ?: continue
            val text = newest.readText()
            val chunks = text.chunked(8000).joinToString(", ") { chunk ->
                "\"" + chunk.flatMap { c ->
                    when (c) {
                        '\\' -> listOf('\\', '\\')
                        '"' -> listOf('\\', '"')
                        '$' -> listOf('\\', '$')
                        '\n' -> listOf('\\', 'n')
                        '\r' -> listOf('\\', 'r')
                        '\t' -> listOf('\\', 't')
                        else -> listOf(c)
                    }
                }.joinToString("") + "\""
            }
            val number = newest.name.removeSuffix(".json").toInt()
            out.append("        \"${app.name}\" to ($number to listOf($chunks).joinToString(\"\")),\n")
        }
        out.append("    )\n}\n")
        val dir = outDir.get().asFile.resolve("org/theconstruct")
        dir.mkdirs()
        dir.resolve("BundledLayoutFiles.kt").writeText(out.toString())
    }
}

tasks.withType<KotlinCompilationTask<*>>().configureEach {
    dependsOn(generateBundledLayouts)
    if (name == "compileKotlinDesktop") dependsOn(buildRustFfi) else mustRunAfter(buildRustFfi)
}
