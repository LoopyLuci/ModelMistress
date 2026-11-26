#include <jni.h>

#include <algorithm>
#include <cstdint>
#include <string>
#include <vector>

#include "cactus.h"

namespace {

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

jstring fromUtf8(JNIEnv* env, const char* bytes, size_t length) {
    std::vector<jchar> result;
    result.reserve(length);
    size_t index = 0;
    while (index < length) {
        const uint8_t first = static_cast<uint8_t>(bytes[index++]);
        uint32_t codePoint;
        size_t continuationCount;
        if (first < 0x80) {
            codePoint = first;
            continuationCount = 0;
        } else if ((first & 0xE0) == 0xC0) {
            codePoint = first & 0x1F;
            continuationCount = 1;
        } else if ((first & 0xF0) == 0xE0) {
            codePoint = first & 0x0F;
            continuationCount = 2;
        } else if ((first & 0xF8) == 0xF0) {
            codePoint = first & 0x07;
            continuationCount = 3;
        } else {
            codePoint = 0xFFFD;
            continuationCount = 0;
        }

        bool valid = index + continuationCount <= length;
        for (size_t count = 0; valid && count < continuationCount; ++count) {
            const uint8_t continuation = static_cast<uint8_t>(bytes[index++]);
            if ((continuation & 0xC0) != 0x80) {
                valid = false;
                break;
            }
            codePoint = (codePoint << 6) | (continuation & 0x3F);
        }
        if (!valid || codePoint > 0x10FFFF ||
            (codePoint >= 0xD800 && codePoint <= 0xDFFF)) {
            codePoint = 0xFFFD;
        }

        if (codePoint <= 0xFFFF) {
            result.push_back(static_cast<jchar>(codePoint));
        } else {
            codePoint -= 0x10000;
            result.push_back(static_cast<jchar>(0xD800 + (codePoint >> 10)));
            result.push_back(static_cast<jchar>(0xDC00 + (codePoint & 0x3FF)));
        }
    }
    return env->NewString(result.data(), static_cast<jsize>(result.size()));
}

void throwCactusError(JNIEnv* env) {
    const char* lastError = cactus_last_error();
    jclass exceptionClass = env->FindClass("java/lang/IllegalStateException");
    env->ThrowNew(
        exceptionClass,
        lastError == nullptr ? "Cactus inference failed." : lastError
    );
}

jstring copyOutput(JNIEnv* env, int result, const std::vector<char>& output) {
    if (result < 0) {
        throwCactusError(env);
        return nullptr;
    }
    const auto terminator = std::find(output.begin(), output.end(), '\0');
    if (terminator == output.end()) {
        jclass exceptionClass = env->FindClass("java/lang/IllegalStateException");
        env->ThrowNew(exceptionClass, "Cactus output exceeded the configured limit.");
        return nullptr;
    }
    const auto length = static_cast<size_t>(terminator - output.begin());
    return fromUtf8(env, output.data(), length);
}

}  // namespace

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_cactus_CactusNative_nativeLoadModel(
    JNIEnv* env,
    jclass,
    jobject modelBuffer
) {
    const auto* model = static_cast<const unsigned char*>(
        env->GetDirectBufferAddress(modelBuffer)
    );
    const jlong length = env->GetDirectBufferCapacity(modelBuffer);
    if (model == nullptr || length <= 0) {
        jclass exceptionClass = env->FindClass("java/lang/IllegalArgumentException");
        env->ThrowNew(exceptionClass, "Model data must be a non-empty direct buffer.");
        return;
    }
    if (cactus_load(model, static_cast<unsigned long long>(length)) < 0) {
        throwCactusError(env);
    }
}

extern "C" JNIEXPORT jint JNICALL
Java_com_modelmistress_cactus_CactusNative_nativeModels(JNIEnv*, jclass) {
    return cactus_models();
}

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_cactus_CactusNative_nativeReset(JNIEnv*, jclass) {
    cactus_reset();
}

extern "C" JNIEXPORT void JNICALL
Java_com_modelmistress_cactus_CactusNative_nativeInitialize(
    JNIEnv* env,
    jclass,
    jstring systemPrompt,
    jstring toolsJson
) {
    const std::string system = toUtf8(env, systemPrompt);
    const std::string tools = toUtf8(env, toolsJson);
    if (cactus_init(
            system.empty() ? nullptr : system.c_str(),
            tools.empty() ? nullptr : tools.c_str(),
            nullptr
        ) < 0) {
        throwCactusError(env);
    }
}

extern "C" JNIEXPORT jstring JNICALL
Java_com_modelmistress_cactus_CactusNative_nativeComplete(
    JNIEnv* env,
    jclass,
    jstring input,
    jint maxNewTokens
) {
    const std::string prompt = toUtf8(env, input);
    std::vector<char> output(1024 * 1024);
    const int result = cactus_complete(
        prompt.c_str(),
        nullptr,
        0,
        maxNewTokens,
        output.data(),
        static_cast<int>(output.size())
    );
    return copyOutput(env, result, output);
}

