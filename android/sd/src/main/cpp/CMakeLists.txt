cmake_minimum_required(VERSION 3.22.1)
project(modelmistress_sd LANGUAGES C CXX)

# android/sd/src/main/cpp -> ModelMistress root -> engines/stable-diffusion.cpp (the vendored sd.cpp this app builds).
set(SD_SOURCE_DIR "${CMAKE_CURRENT_LIST_DIR}/../../../../../engines/stable-diffusion.cpp")
if(NOT EXISTS "${SD_SOURCE_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "stable-diffusion.cpp was not found at ${SD_SOURCE_DIR}. Check out engines/stable-diffusion.cpp first.")
endif()

# Built optimised, as the llama.cpp engine is: AGP's debug default would run several times slower on a phone.
set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type" FORCE)

# sd.cpp vendors ggml, and so does the llama.cpp engine. Hidden symbols keep this copy inside the bridge library, so the
# two copies never meet in one process. Only the JNI functions, which are marked default, are exported.
set(CMAKE_C_VISIBILITY_PRESET hidden)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)

# The NDK toolchain puts -g on every compile line, so each object carries DWARF debug info. AGP strips it when it packages
# the libraries, but stripping failed for this one and the APK shipped about 370 MB of debug info. Drop -g from the flags
# of this build (and of the sd.cpp and ggml subtree below). The optimisation level is unchanged.
foreach(lang IN ITEMS C CXX)
    string(REGEX REPLACE "(^| )-g( |$)" " " CMAKE_${lang}_FLAGS "${CMAKE_${lang}_FLAGS}")
endforeach()

set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(SD_BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(SD_BUILD_SHARED_GGML_LIB OFF CACHE BOOL "" FORCE)
set(SD_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(SD_WEBP OFF CACHE BOOL "" FORCE)
set(SD_WEBM OFF CACHE BOOL "" FORCE)
add_subdirectory("${SD_SOURCE_DIR}" "${CMAKE_CURRENT_BINARY_DIR}/stable-diffusion")

add_library(stable_diffusion_bridge SHARED stable_diffusion_jni.cpp)
target_compile_features(stable_diffusion_bridge PRIVATE cxx_std_17)
target_include_directories(stable_diffusion_bridge PRIVATE "${SD_SOURCE_DIR}/include")
target_link_libraries(stable_diffusion_bridge PRIVATE stable-diffusion android log)
