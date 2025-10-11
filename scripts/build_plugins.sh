#!/bin/bash
# Build script for compiling plugins as dynamic libraries

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}Building plugins...${NC}"

# Get the script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Plugin directory
PLUGINS_DIR="$PROJECT_ROOT/crates/plugins"

# Output directory for compiled plugins
OUTPUT_DIR="$PROJECT_ROOT/target/plugins"
mkdir -p "$OUTPUT_DIR"

profile="${1:-debug}"
echo $profile

# Build each plugin
for plugin_dir in "$PLUGINS_DIR"/*; do
  if [ -d "$plugin_dir" ]; then
    plugin_name=$(basename "$plugin_dir")
    echo -e "${GREEN}Building plugin: $plugin_name${NC}"

    cd "$plugin_dir"

    # Set up linker flags for undefined symbols
    if [[ "$OSTYPE" == "darwin"* ]]; then
      export RUSTFLAGS="-C link-arg=-undefined -C link-arg=dynamic_lookup"
    elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
      export RUSTFLAGS="-C link-arg=-Wl,--allow-shlib-undefined"
    fi

    cargo build $1

    # Check if plugin built successfully
    # Handle different platforms
    if [[ "$OSTYPE" == "darwin"* ]]; then
      # macOS
      lib_file="target/$profile/lib${plugin_name}_plugin.dylib"
    elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
      # Linux
      lib_file="target/$profile/lib${plugin_name}_plugin.so"
    elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
      # Windows
      lib_file="target/$profile/${plugin_name}_plugin.dll"
    else
      echo -e "${RED}Unsupported platform: $OSTYPE${NC}"
      exit 1
    fi

    if [ -f "$lib_file" ]; then
      cp "$lib_file" "$PROJECT_ROOT/$lib_file"
      echo -e "${GREEN}  ✓ Built plugin at: $lib_file${NC}"

      # Copy plugin.toml to release directory with plugin name
      if [ -f "$plugin_dir/plugin.toml" ]; then
        mkdir -p "$PROJECT_ROOT/target/plugins/$profile"
        cp "$plugin_dir/plugin.toml" "$PROJECT_ROOT/target/plugins/$profile/${plugin_name}.toml"
        echo -e "${GREEN}  ✓ Copied plugin.toml to ${profile} directory as ${plugin_name}.toml${NC}"
      fi
    else
      echo -e "${RED}  ✗ Failed to find library at: $lib_file${NC}"
    fi
  fi
done

echo -e "${GREEN}Plugin build complete!${NC}"
echo -e "Plugins available in: $OUTPUT_DIR"
