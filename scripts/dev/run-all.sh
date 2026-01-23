#!/usr/bin/env bash
set -euo pipefail

export AADK_JOB_ADDR="${AADK_JOB_ADDR:-127.0.0.1:50051}"
export AADK_TOOLCHAIN_ADDR="${AADK_TOOLCHAIN_ADDR:-127.0.0.1:50052}"
export AADK_PROJECT_ADDR="${AADK_PROJECT_ADDR:-127.0.0.1:50053}"
export AADK_BUILD_ADDR="${AADK_BUILD_ADDR:-127.0.0.1:50054}"
export AADK_TARGETS_ADDR="${AADK_TARGETS_ADDR:-127.0.0.1:50055}"
export AADK_OBSERVE_ADDR="${AADK_OBSERVE_ADDR:-127.0.0.1:50056}"
export AADK_WORKFLOW_ADDR="${AADK_WORKFLOW_ADDR:-127.0.0.1:50057}"

pick_latest_dir() {
  local base="$1"
  local latest
  latest=$(ls -1dt "$base"/*/ 2>/dev/null | head -n 1 || true)
  if [ -n "$latest" ]; then
    printf '%s' "${latest%/}"
    return 0
  fi
  return 1
}

is_valid_sdk() {
  local sdk="$1"
  if [ -x "$sdk/platform-tools/adb" ] || [ -x "$sdk/platform-tools/adb.exe" ]; then
    return 0
  fi
  return 1
}

is_valid_ndk() {
  local ndk="$1"
  if [ -f "$ndk/source.properties" ] && [ -d "$ndk/toolchains/llvm" ]; then
    return 0
  fi
  return 1
}

pick_latest_valid_sdk() {
  local base="$1"
  local dir
  while read -r dir; do
    dir="${dir%/}"
    if is_valid_sdk "$dir"; then
      printf '%s' "$dir"
      return 0
    fi
  done < <(ls -1dt "$base"/*/ 2>/dev/null || true)
  return 1
}

pick_latest_valid_ndk() {
  local base="$1"
  local dir
  while read -r dir; do
    dir="${dir%/}"
    if is_valid_ndk "$dir"; then
      printf '%s' "$dir"
      return 0
    fi
  done < <(ls -1dt "$base"/*/ 2>/dev/null || true)
  return 1
}

java_major_version() {
  local java_bin="$1"
  local version_line
  version_line=$("$java_bin" -version 2>&1 | head -n 1)
  version_line=${version_line#*\"}
  version_line=${version_line%%\"*}
  printf '%s' "${version_line%%.*}"
}

find_supported_java() {
  local desired
  local candidate
  for desired in 21 17; do
    for candidate in /usr/lib/jvm/*; do
      if [ -x "$candidate/bin/java" ]; then
        if [ "$(java_major_version "$candidate/bin/java")" = "$desired" ]; then
          printf '%s' "$candidate"
          return 0
        fi
      fi
    done
  done
  return 1
}

if [ -z "${ANDROID_SDK_ROOT:-}" ]; then
  for base in "$HOME/.local/share/aadk/toolchains/android-sdk-custom" "$HOME/Android/Sdk" "$HOME/Android/sdk"; do
    if sdk_path=$(pick_latest_valid_sdk "$base"); then
      export ANDROID_SDK_ROOT="$sdk_path"
      export ANDROID_HOME="$sdk_path"
      break
    fi
  done
fi

if [ -z "${ANDROID_NDK_ROOT:-}" ]; then
  for base in "$HOME/.local/share/aadk/toolchains/android-ndk-custom" "${ANDROID_SDK_ROOT:-}/ndk" "${ANDROID_SDK_ROOT:-}/ndk-bundle"; do
    if ndk_path=$(pick_latest_valid_ndk "$base"); then
      export ANDROID_NDK_ROOT="$ndk_path"
      export ANDROID_NDK_HOME="$ndk_path"
      break
    fi
  done
fi

if [ -n "${AADK_JAVA_HOME:-}" ]; then
  export JAVA_HOME="$AADK_JAVA_HOME"
fi

if [ -z "${JAVA_HOME:-}" ]; then
  if supported_java=$(find_supported_java); then
    export JAVA_HOME="$supported_java"
  elif command -v javac >/dev/null 2>&1; then
    javac_path=$(readlink -f "$(command -v javac)")
    export JAVA_HOME="$(dirname "$(dirname "$javac_path")")"
  else
    for candidate in /usr/lib/jvm/*; do
      if [ -x "$candidate/bin/java" ]; then
        export JAVA_HOME="$candidate"
        break
      fi
    done
  fi
fi

if [ -n "${JAVA_HOME:-}" ] && [ -x "$JAVA_HOME/bin/java" ]; then
  java_major=$(java_major_version "$JAVA_HOME/bin/java")
  if [ "$java_major" != "17" ] && [ "$java_major" != "21" ]; then
    if supported_java=$(find_supported_java); then
      echo "WARN: JAVA_HOME points to Java $java_major; switching to $supported_java for AGP 8.x."
      export JAVA_HOME="$supported_java"
      java_major=$(java_major_version "$JAVA_HOME/bin/java")
    else
      echo "WARN: JAVA_HOME points to Java $java_major, but AGP 8.x expects Java 17 or 21."
    fi
  fi
fi

if [ -n "${JAVA_HOME:-}" ] && [[ ":$PATH:" != *":$JAVA_HOME/bin:"* ]]; then
  export PATH="$JAVA_HOME/bin:$PATH"
fi

if [ -n "${ANDROID_SDK_ROOT:-}" ] && [ -d "$ANDROID_SDK_ROOT/platform-tools" ]; then
  if [[ ":$PATH:" != *":$ANDROID_SDK_ROOT/platform-tools:"* ]]; then
    export PATH="$ANDROID_SDK_ROOT/platform-tools:$PATH"
  fi
fi

if [ -z "${AADK_ADB_PATH:-}" ] && [ -n "${ANDROID_SDK_ROOT:-}" ]; then
  if [ -x "$ANDROID_SDK_ROOT/platform-tools/adb" ]; then
    export AADK_ADB_PATH="$ANDROID_SDK_ROOT/platform-tools/adb"
  elif [ -x "$ANDROID_SDK_ROOT/platform-tools/adb.exe" ]; then
    export AADK_ADB_PATH="$ANDROID_SDK_ROOT/platform-tools/adb.exe"
  fi
fi

echo "Environment:"
echo "  ANDROID_SDK_ROOT=${ANDROID_SDK_ROOT:-<unset>}"
echo "  ANDROID_NDK_ROOT=${ANDROID_NDK_ROOT:-<unset>}"
echo "  JAVA_HOME=${JAVA_HOME:-<unset>}"
echo "  AADK_ADB_PATH=${AADK_ADB_PATH:-<unset>}"
echo

if [ -z "${ANDROID_SDK_ROOT:-}" ]; then
  echo "WARN: ANDROID_SDK_ROOT not set. Install the SDK via Toolchains or set ANDROID_SDK_ROOT."
elif ! is_valid_sdk "$ANDROID_SDK_ROOT"; then
  echo "WARN: ANDROID_SDK_ROOT does not look like a full SDK (missing platform-tools/adb)."
fi

if [ -z "${JAVA_HOME:-}" ]; then
  echo "WARN: JAVA_HOME not set. Install a JDK (e.g. openjdk-17-jdk)."
fi
echo

pids=()

cleanup() {
  echo "Stopping services..."
  for pid in "${pids[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait || true
}
trap cleanup EXIT INT TERM

echo "Starting aadk-core (JobService) on $AADK_JOB_ADDR"
cargo run -p aadk-core --quiet &
pids+=($!)

echo "Starting aadk-toolchain on $AADK_TOOLCHAIN_ADDR"
cargo run -p aadk-toolchain --quiet &
pids+=($!)

echo "Starting aadk-project on $AADK_PROJECT_ADDR"
cargo run -p aadk-project --quiet &
pids+=($!)

echo "Starting aadk-build on $AADK_BUILD_ADDR"
cargo run -p aadk-build --quiet &
pids+=($!)

echo "Starting aadk-targets on $AADK_TARGETS_ADDR"
cargo run -p aadk-targets --quiet &
pids+=($!)

echo "Starting aadk-observe on $AADK_OBSERVE_ADDR"
cargo run -p aadk-observe --quiet &
pids+=($!)

echo "Starting aadk-workflow on $AADK_WORKFLOW_ADDR"
cargo run -p aadk-workflow --quiet &
pids+=($!)

echo
echo "All services started."
echo "Press Ctrl+C to stop."
echo

wait
