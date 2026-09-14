import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("jvm")
    kotlin("plugin.serialization")
}

// JVM puro, sem o plugin do Android, de proposito: este modulo e a logica
// do protocolo, e tem que rodar em `./gradlew test` numa maquina sem
// aparelho nem emulador. O modulo Android vai depender dele.
//
// Sem `jvmToolchain`: compila com o JDK que estiver disponivel e so fixa o
// *alvo* em 17, que e o que o Android aceita. Exigir um JDK 17 instalado
// faria o build falhar em maquina que so tem o JBR do Android Studio.
kotlin {
    compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

repositories { mavenCentral() }

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.3")
    // OkHttp e nao java.net.http: `java.net.http` nao existe no Android, e
    // este codigo vai inteiro pro app.
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation(kotlin("test"))
}

tasks.test { useJUnitPlatform() }
