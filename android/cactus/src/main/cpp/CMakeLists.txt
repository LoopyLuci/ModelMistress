cmake_minimum_required(VERSION 3.22.1)

project(modelmistress_cactus LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Built optimised, as the other native engines are.
set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type" FORCE)

# The NDK toolchain puts -g on every compile line. AGP strips it when it packages the libraries, but the stripping failed
# for the stable-diffusion bridge and shipped about 370 MB of debug info. Drop -g here as well; the optimisation is unchanged.
foreach(lang IN ITEMS C CXX)
    string(REGEX REPLACE "(^| )-g( |$)" " " CMAKE_${lang}_FLAGS "${CMAKE_${lang}_FLAGS}")
endforeach()

# The Cactus runtime, prebuilt for arm64-v8a from MobileAgents' core/inference module. It defines the cactus_* C API
# that cactus.h declares and no ggml or llama symbols, so it does not clash with the llama.cpp engine in the same process.
add_library(cactus_static STATIC IMPORTED)
set_target_properties(cactus_static PROPERTIES
    IMPORTED_LOCATION "${CMAKE_CURRENT_SOURCE_DIR}/prebuilt/${ANDROID_ABI}/libcactus.a"
)

add_library(cactus_bridge SHARED cactus_jni.cpp)
target_include_directories(cactus_bridge PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_compile_options(cactus_bridge PRIVATE -fexceptions -frtti)
target_link_libraries(cactus_bridge PRIVATE cactus_static android log m)
