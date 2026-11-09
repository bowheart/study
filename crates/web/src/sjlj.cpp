// The prebuilt wasm Skia from rust-skia (FreeType, libpng, libjpeg) was compiled for Emscripten's
// JavaScript setjmp/longjmp: calls that may longjmp go through `invoke_*` imports, and
// `emscripten_longjmp` throws a JavaScript exception that those imports catch. Rust links with
// WebAssembly exception handling, which rules that JavaScript runtime out, so this provides the
// same protocol on top of wasm exceptions. build.rs passes this file to em++ at link time.

#include <stdint.h>
#include <type_traits>

extern "C" void setThrew(uintptr_t threw, int value);

namespace {
struct Longjmp {};

template <typename R, typename... A>
R invoke(R (*f)(A...), A... a) {
    try {
        return f(a...);
    } catch (Longjmp&) {
        // The frame that called setjmp checks __THREW__ (set by emscripten_longjmp) next.
        setThrew(1, 0);
        if constexpr (!std::is_void_v<R>) {
            return R();
        }
    }
}
}  // namespace

extern "C" void emscripten_longjmp(uintptr_t env, int val) {
    setThrew(env, val == 0 ? 1 : val);
    throw Longjmp{};
}

extern "C" int invoke_ii(int (*f)(int), int a1) { return invoke(f, a1); }
extern "C" int invoke_iii(int (*f)(int, int), int a1, int a2) { return invoke(f, a1, a2); }
extern "C" int invoke_iiii(int (*f)(int, int, int), int a1, int a2, int a3) { return invoke(f, a1, a2, a3); }
extern "C" int invoke_iiiii(int (*f)(int, int, int, int), int a1, int a2, int a3, int a4) { return invoke(f, a1, a2, a3, a4); }
extern "C" int invoke_iiiiiii(int (*f)(int, int, int, int, int, int), int a1, int a2, int a3, int a4, int a5, int a6) { return invoke(f, a1, a2, a3, a4, a5, a6); }
extern "C" int invoke_iiiiiiii(int (*f)(int, int, int, int, int, int, int), int a1, int a2, int a3, int a4, int a5, int a6, int a7) { return invoke(f, a1, a2, a3, a4, a5, a6, a7); }
extern "C" int invoke_iiiiiiiiii(int (*f)(int, int, int, int, int, int, int, int, int), int a1, int a2, int a3, int a4, int a5, int a6, int a7, int a8, int a9) { return invoke(f, a1, a2, a3, a4, a5, a6, a7, a8, a9); }
extern "C" void invoke_v(void (*f)()) { return invoke(f); }
extern "C" void invoke_vi(void (*f)(int), int a1) { return invoke(f, a1); }
extern "C" void invoke_vii(void (*f)(int, int), int a1, int a2) { return invoke(f, a1, a2); }
extern "C" void invoke_viii(void (*f)(int, int, int), int a1, int a2, int a3) { return invoke(f, a1, a2, a3); }
extern "C" void invoke_viiii(void (*f)(int, int, int, int), int a1, int a2, int a3, int a4) { return invoke(f, a1, a2, a3, a4); }
extern "C" void invoke_viiiii(void (*f)(int, int, int, int, int), int a1, int a2, int a3, int a4, int a5) { return invoke(f, a1, a2, a3, a4, a5); }
extern "C" void invoke_viiiiii(void (*f)(int, int, int, int, int, int), int a1, int a2, int a3, int a4, int a5, int a6) { return invoke(f, a1, a2, a3, a4, a5, a6); }
extern "C" void invoke_viiiiiiiii(void (*f)(int, int, int, int, int, int, int, int, int), int a1, int a2, int a3, int a4, int a5, int a6, int a7, int a8, int a9) { return invoke(f, a1, a2, a3, a4, a5, a6, a7, a8, a9); }
