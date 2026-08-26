"""Patch libsodium-sys 0.2.7 build.rs for OHOS cross-compilation.

- HOST side (hbb_common pulled via rustdesk [build-dependencies]): emit no
  link directives at all; unreferenced extern "C" declarations are dropped by
  the linker, so the host build script links without a host libsodium.
- CROSS side (OHOS target): require SODIUM_LIB_DIR pointing at the prebuilt
  static lib; never fall back to the vendored autotools build (config.sub does
  not recognise the OHOS triple).
"""
import glob
import os
import re
import sys

GUARD_MARK = "__OHOS_HOST_GUARD__"

GUARD = '''fn main() {
    // __OHOS_HOST_GUARD__
    let cross_target = env::var("TARGET").unwrap_or_default();
    let host_target = env::var("HOST").unwrap_or_default();
    if cross_target != host_target {
        if env::var("SODIUM_LIB_DIR").is_ok() {
            find_libsodium_env();
        } else {
            panic!("SODIUM_LIB_DIR must be set for OHOS cross-compile of libsodium-sys");
        }
        return;
    }
    // Host build: no link emission needed.
}
'''

def patch(path):
    src = open(path, encoding='utf-8').read()
    if GUARD_MARK in src:
        print('already patched:', path)
        return False
    m = re.search(r'fn main\(\) \{.*?\n\}', src, re.DOTALL)
    if not m:
        raise SystemExit('main() not found in ' + path)
    src = src[:m.start()] + GUARD + src[m.end():]
    open(path, 'w', encoding='utf-8').write(src)
    print('patched:', path)
    return True

def main():
    patterns = sys.argv[1:] or [
        os.path.expanduser('~/.cargo/registry/src/*/libsodium-sys-0.2.7/build.rs'),
    ]
    patched = 0
    for pattern in patterns:
        for path in glob.glob(pattern):
            patched += bool(patch(path))
    if patched == 0:
        print('warning: no build.rs patched')

if __name__ == '__main__':
    main()
