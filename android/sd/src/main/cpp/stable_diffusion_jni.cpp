#include <jni.h>

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <limits>
#include <memory>
#include <mutex>
#include <shared_mutex>
#include <string>

#include "stable-diffusion.h"

namespace {

std::shared_mutex contextMutex;
std::mutex generationMutex;
std::atomic<sd_ctx_t*> activeContext{nullptr};

std::string toUtf8(JNIEnv* env, jstring value) {
    if (value == nullptr) return {};
    const jsize length = env->GetStringLength(value);
    const jchar* chars = env->GetStringChars(value, nullptr);
    if (chars == nullptr) return {};
    std::string result;
    result.reserve(static_cast<size_t>(length) * 3);
    for (jsize index = 0; index < length; ++index) {
        uint32_t codePoint = chars[index];
        if (codePoint >= 0xD800 && codePoint <= 0xDBFF &&
            index + 1 < length && chars[index + 1] >= 0xDC00 &&
            chars[index + 1] <= 0xDFFF) {
            codePoint = 0x10000 + ((codePoint - 0xD800) << 10) +
                        (chars[++index] - 0xDC00);
        } else if (codePoint >= 0xD800 && codePoint <= 0xDFFF) {
            codePoint = 0xFFFD;
        }

        if (codePoint <= 0x7F) {
            result.push_back(static_cast<char>(codePoint));
        } else if (codePoint <= 0x7FF) {
            result.push_back(static_cast<char>(0xC0 | (codePoint >> 6)));
            result.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
        } else if (codePoint <= 0xFFFF) {
            result.push_back(static_cast<char>(0xE0 | (codePoint >> 12)));
            result.push_back(static_cast<char>(0x80 | ((codePoint >> 6) & 0x3F)));
            result.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
        } else {
            result.push_back(static_cast<char>(0xF0 | (codePoint >> 18)));
            result.push_back(static_cast<char>(0x80 | ((codePoint >> 12) & 0x3F)));
            result.push_back(static_cast<char>(0x80 | ((codePoint >> 6) & 0x3F)));
            result.push_back(static_cast<char>(0x80 | (codePoint & 0x3F)));
        }
    }
    env->ReleaseStringChars(value, chars);
    return result;
}

void throwJava(JNIEnv* env, const char* className, const std::string& message) {
    if (env->ExceptionCheck()) return;
    jclass exceptionClass = env->FindClass(className);
    if (exceptionClass != nullptr) {
        env->ThrowNew(exceptionClass, message.c_str());
    }
}

void throwSdError(JNIEnv* env, const char* fallback) {
    const char* lastError = sd_get_last_error();
    throwJava(
        env,
        "java/lang/IllegalStateException",
        lastError == nullptr ? fallback : lastError
    );
}

bool setString(
    JNIEnv* env,
    sd_opt_t* options,
    const char* key,
    const std::string& value
) {
    const sd_status_t status = sd_opt_set_string(options, key, value.c_str());
    if (status == SD_STATUS_OK) return true;
    throwSdError(env, "Could not configure image generation.");
    return false;
}

bool setInt(
    JNIEnv* env,
    sd_opt_t* options,
    const char* key,
    int64_t value
) {
    const sd_status_t status = sd_opt_set_int(options, key, value);
    if (status == SD_STATUS_OK) return true;
    throwSdError(env, "Could not configure image generation.");
    return false;
}

struct ProgressCallback {
    JNIEnv* env;
    jobject listener;
    jmethodID method;
};

void onProgress(const sd_generation_progress_t* progress, void* data) {
    if (progress == nullptr || data == nullptr) return;
    auto* callback = static_cast<ProgressCallback*>(data);
    callback->env->CallVoidMethod(
        callback->listener,
        callback->method,
        static_cast<jint>(std::clamp<int64_t>(
            progress->completed,
            std::numeric_limits<jint>::min(),
            std::numeric_limits<jint>::max()
        )),
        static_cast<jint>(std::clamp<int64_t>(
            progress->total,
            std::numeric_limits<jint>::min(),
            std::numeric_limits<jint>::max()
        ))
    );
}

}  // namespace

extern "C" JNIEXPORT jstring JNICALL
Java_com_modelmistress_sd_StableDiffusionNative_nativeVersion(
    JNIEnv* env,
    jclass
) {
    const char* commit = sd_commit();
    return env->NewStringUTF(commit == nullptr ? "unknown" : commit);
}

extern "C" JNIEXPORT jlong JNICALL
Java_com_modelmistress_sd_StableDiffusionNative_nativeCreate(
    JNIEnv* env,
    jclass,
    jstring modelPath,
    jint threadCount
) {
    const std::string path = toUtf8(env, modelPath);
    if (path.empty() || threadCount < 1) {
        throwJava(
            env,
            "java/lang/IllegalArgumentException",
            "A model path and positive thread count are required."
        );
        return 0;
    }

    std::unique_lock<std::shared_mutex> lock(contextMutex);
    std::unique_ptr<sd_ctx_opt_t, decltype(&sd_opt_free)> options(
        sd_ctx_opt_create(),
        sd_opt_free
    );
    if (!options) {
        throwJava(env, "java/lang/OutOfMemoryError", "Could not allocate model options.");
        return 0;
    }
    if (!setString(env, options.get(), "model_path", path) ||
        !setInt(env, options.get(), "n_threads", threadCount)) {
        return 0;
    }

    sd_ctx_t* context = nullptr;
    const sd_status_t status = sd_ctx_create(options.get(), &context);
    if (status != SD_STATUS_OK || context == nullptr) {
        throwSdError(env, "Could not load the image-generation model.");
        return 0;
    }
    return static_cast<jlong>(reinterpret_cast<intptr_t>(context));
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_com_modelmistress_sd_StableDiffusionNative_nativeGenerate(
    JNIEnv* env,
    jclass,
    jlong handle,
    jstring prompt,
    jstring negativePrompt,
    jint width,
    jint height,
    jlong seed,
    jint steps,
    jobject progressListener
) {
    if (handle == 0 || prompt == nullptr || width < 64 || height < 64 ||
        width > 2048 || height > 2048 || steps < 1 || steps > 100) {
        throwJava(
            env,
            "java/lang/IllegalArgumentException",
            "Image generation parameters are outside the supported bounds."
        );
        return nullptr;
    }

    std::shared_lock<std::shared_mutex> contextLock(contextMutex);
    std::lock_guard<std::mutex> generationLock(generationMutex);
    auto* context = reinterpret_cast<sd_ctx_t*>(static_cast<intptr_t>(handle));
    sd_ctx_t* expected = nullptr;
    if (!activeContext.compare_exchange_strong(expected, context)) {
        throwJava(
            env,
            "java/lang/IllegalStateException",
            "Another image-generation request is active."
        );
        return nullptr;
    }

    const std::string promptText = toUtf8(env, prompt);
    const std::string negativeText = toUtf8(env, negativePrompt);
    std::unique_ptr<sd_img_gen_opt_t, decltype(&sd_opt_free)> options(
        sd_img_gen_opt_create(),
        sd_opt_free
    );
    if (!options) {
        activeContext.store(nullptr);
        throwJava(env, "java/lang/OutOfMemoryError", "Could not allocate generation options.");
        return nullptr;
    }

    if (!setString(env, options.get(), "prompt", promptText) ||
        !setString(env, options.get(), "negative_prompt", negativeText) ||
        !setInt(env, options.get(), "width", width) ||
        !setInt(env, options.get(), "height", height) ||
        !setInt(env, options.get(), "seed", seed) ||
        !setInt(env, options.get(), "sample.steps", steps)) {
        activeContext.store(nullptr);
        return nullptr;
    }

    ProgressCallback callback{env, progressListener, nullptr};
    if (progressListener != nullptr) {
        jclass listenerClass = env->GetObjectClass(progressListener);
        callback.method = listenerClass == nullptr
            ? nullptr
            : env->GetMethodID(listenerClass, "onProgress", "(II)V");
        if (callback.method == nullptr) {
            activeContext.store(nullptr);
            throwJava(
                env,
                "java/lang/IllegalArgumentException",
                "The progress listener has no onProgress(int, int) method."
            );
            return nullptr;
        }
        sd_set_generation_progress_callback(context, onProgress, &callback);
    }

    sd_result_t* rawResult = nullptr;
    const sd_status_t status = sd_generate_image(context, options.get(), &rawResult);
    activeContext.store(nullptr);
    sd_set_generation_progress_callback(context, nullptr, nullptr);
    std::unique_ptr<sd_result_t, decltype(&sd_result_free)> result(rawResult, sd_result_free);
    if (status == SD_STATUS_CANCELLED) {
        throwJava(
            env,
            "java/util/concurrent/CancellationException",
            "Image generation was cancelled."
        );
        return nullptr;
    }
    if (status != SD_STATUS_OK || !result) {
        throwSdError(env, "Image generation failed.");
        return nullptr;
    }

    const sd_tensor_t* image = sd_result_get_image(result.get(), 0);
    if (image == nullptr || sd_tensor_type(image) != SD_TENSOR_U8 ||
        sd_tensor_rank(image) != 3 || sd_tensor_dim(image, 0) != 3 ||
        sd_tensor_dim(image, 1) != width || sd_tensor_dim(image, 2) != height) {
        throwJava(
            env,
            "java/lang/IllegalStateException",
            "The image runtime returned an unsupported image layout."
        );
        return nullptr;
    }

    const size_t byteCount = sd_tensor_byte_size(image);
    const void* pixels = sd_tensor_data(image);
    if (pixels == nullptr || byteCount > static_cast<size_t>(std::numeric_limits<jsize>::max())) {
        throwJava(
            env,
            "java/lang/IllegalStateException",
            "The generated image exceeds Android's supported array size."
        );
        return nullptr;
    }
    jbyteArray output = env->NewByteArray(static_cast<jsize>(byteCount));
    if (output != nullptr) {
        env->SetByteArrayRegion(
            output,
            0,
            static_cast<jsize>(byteCount),
            static_cast<const jbyte*>(pixels)
        );
    }
    return output;
}

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_sd_StableDiffusionNative_nativeCancel(
    JNIEnv*,
    jclass,
    jlong handle
) {
    std::shared_lock<std::shared_mutex> lock(contextMutex);
    auto* context = reinterpret_cast<sd_ctx_t*>(static_cast<intptr_t>(handle));
    if (context != nullptr && activeContext.load() == context) {
        sd_cancel_generation(context, SD_CANCEL_ALL);
    }
}

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_sd_StableDiffusionNative_nativeClose(
    JNIEnv*,
    jclass,
    jlong handle
) {
    if (handle == 0) return;
    std::unique_lock<std::shared_mutex> lock(contextMutex);
    auto* context = reinterpret_cast<sd_ctx_t*>(static_cast<intptr_t>(handle));
    if (activeContext.load() == context) return;
    sd_ctx_free(context);
}
