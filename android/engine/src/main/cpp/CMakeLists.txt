cmake_minimum_required(VERSION 3.22.1)
project(modelmistress_llama LANGUAGES C CXX)

# android/engine/src/main/cpp -> ModelMistress root -> engines/llama.cpp (the vendored llama.cpp this app builds).
set(LLAMA_SOURCE_DIR "${CMAKE_CURRENT_LIST_DIR}/../../../../../engines/llama.cpp")
if(NOT EXISTS "${LLAMA_SOURCE_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "llama.cpp was not found at ${LLAMA_SOURCE_DIR}. Check out engines/llama.cpp first.")
endif()

# Debug builds (AGP's default) compile llama.cpp at -O0, which runs 5 to 10 times slower on a phone. The inference code is
# always built optimised; the Kotlin side can still be debugged.
set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type" FORCE)

set(LLAMA_BUILD_IS_DEV OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
add_subdirectory("${LLAMA_SOURCE_DIR}" "${CMAKE_CURRENT_BINARY_DIR}/llama.cpp")

add_library(modelmistress_llama SHARED llama_jni.cpp)
target_compile_features(modelmistress_llama PRIVATE cxx_std_17)
target_include_directories(modelmistress_llama PRIVATE "${LLAMA_SOURCE_DIR}/include")
target_link_libraries(modelmistress_llama PRIVATE llama android log)
