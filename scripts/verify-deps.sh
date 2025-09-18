#!/bin/bash

# Dependency Installation and Verification Script for video-storage
# This script automatically detects and installs missing dependencies
# Supports: Ubuntu 22.04, Rocky Linux 9

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Script configuration
SCRIPT_VERSION="1.0.0"
DRY_RUN=false
VERBOSE=false
FORCE_INSTALL=false
CHECK_ONLY=false

# Track installation status
DEPS_INSTALLED=0
DEPS_ALREADY_PRESENT=0
DEPS_FAILED=0

# Print colored message
print_msg() {
    local color=$1
    shift
    echo -e "${color}$*${NC}"
}

# Print section header
print_header() {
    echo ""
    echo "========================================="
    print_msg "$BOLD" "$1"
    echo "========================================="
}

# Print usage information
usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Automatically detect and install dependencies for video-storage project.

OPTIONS:
    -h, --help          Show this help message
    -v, --verbose       Show detailed output
    -n, --dry-run       Show what would be installed without actually installing
    -f, --force         Force reinstall all dependencies
    -c, --check-only    Only check dependencies, don't install
    --version           Show script version

EXAMPLES:
    $0                  # Install missing dependencies
    $0 --dry-run        # Show what would be installed
    $0 --check-only     # Only check, don't install
    $0 --force          # Reinstall all dependencies

SUPPORTED SYSTEMS:
    - Ubuntu 22.04
    - Rocky Linux 9

EOF
    exit 0
}

# Parse command line arguments
parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            -h|--help)
                usage
                ;;
            -v|--verbose)
                VERBOSE=true
                shift
                ;;
            -n|--dry-run)
                DRY_RUN=true
                shift
                ;;
            -f|--force)
                FORCE_INSTALL=true
                shift
                ;;
            -c|--check-only)
                CHECK_ONLY=true
                shift
                ;;
            --version)
                echo "verify-deps.sh version $SCRIPT_VERSION"
                exit 0
                ;;
            *)
                print_msg "$RED" "Unknown option: $1"
                echo "Use -h for help"
                exit 1
                ;;
        esac
    done
}

# Detect operating system
detect_os() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        OS_NAME=$ID
        OS_VERSION=$VERSION_ID
        OS_PRETTY_NAME=$PRETTY_NAME
    else
        print_msg "$RED" "Cannot detect operating system"
        exit 1
    fi

    # Normalize OS detection
    case "$OS_NAME" in
        ubuntu)
            if [[ "$OS_VERSION" != "22.04" ]]; then
                print_msg "$YELLOW" "Warning: This script is tested on Ubuntu 22.04, you have $OS_VERSION"
            fi
            PKG_MANAGER="apt"
            ;;
        rocky|rhel|centos|almalinux)
            if [[ "$OS_VERSION" != "9"* ]]; then
                print_msg "$YELLOW" "Warning: This script is tested on Rocky Linux 9, you have $OS_VERSION"
            fi
            PKG_MANAGER="dnf"
            ;;
        *)
            print_msg "$RED" "Unsupported operating system: $OS_NAME"
            print_msg "$YELLOW" "This script supports Ubuntu 22.04 and Rocky Linux 9"
            exit 1
            ;;
    esac

    print_msg "$BLUE" "Detected OS: $OS_PRETTY_NAME"
    print_msg "$BLUE" "Package Manager: $PKG_MANAGER"
}

# Check if running with sudo (when needed)
check_sudo() {
    if [ "$DRY_RUN" = true ] || [ "$CHECK_ONLY" = true ]; then
        return 0
    fi

    if [ "$EUID" -ne 0 ] && ! sudo -n true 2>/dev/null; then
        print_msg "$YELLOW" "This script needs sudo privileges to install packages."
        print_msg "$YELLOW" "Please enter your password when prompted."
        sudo -v
        if [ $? -ne 0 ]; then
            print_msg "$RED" "Failed to obtain sudo privileges"
            exit 1
        fi
    fi
}

# Execute command with proper privileges
exec_cmd() {
    local cmd="$*"

    if [ "$DRY_RUN" = true ]; then
        print_msg "$YELLOW" "[DRY RUN] Would execute: $cmd"
        return 0
    fi

    if [ "$VERBOSE" = true ]; then
        if [ "$EUID" -ne 0 ]; then
            sudo $cmd
        else
            $cmd
        fi
    else
        if [ "$EUID" -ne 0 ]; then
            sudo $cmd > /dev/null 2>&1
        else
            $cmd > /dev/null 2>&1
        fi
    fi
}

# Check if a command exists
check_command() {
    local cmd=$1
    if command -v "$cmd" &> /dev/null; then
        [ "$VERBOSE" = true ] && print_msg "$GREEN" "  ✓ $cmd found: $(command -v $cmd)"
        return 0
    else
        [ "$VERBOSE" = true ] && print_msg "$RED" "  ✗ $cmd not found"
        return 1
    fi
}

# Check if a package is installed (Ubuntu)
check_apt_package() {
    local pkg=$1
    if dpkg -s "$pkg" >/dev/null 2>&1; then
        return 0
    else
        return 1
    fi
}

# Check if a package is installed (Rocky)
check_dnf_package() {
    local pkg=$1
    if rpm -q "$pkg" &> /dev/null; then
        return 0
    else
        return 1
    fi
}

# Install packages for Ubuntu
install_ubuntu_deps() {
    local packages=(
        build-essential
        gcc
        git
        clang
        libavcodec-dev
        libavformat-dev
        libavutil-dev
        libswscale-dev
        libavfilter-dev
        libavdevice-dev
        pkg-config
        nasm
        libopus-dev
        libvpx-dev
        libx264-dev
        libx265-dev
        libvorbis-dev
        libmp3lame-dev
        libass-dev
        libssl-dev
    )

    local to_install=()

    print_msg "$BOLD" "Checking Ubuntu packages..."

    for pkg in "${packages[@]}"; do
        if [ "$FORCE_INSTALL" = true ] || ! check_apt_package "$pkg"; then
            to_install+=("$pkg")
            [ "$VERBOSE" = true ] && print_msg "$YELLOW" "  Will install: $pkg"
        else
            DEPS_ALREADY_PRESENT=$((DEPS_ALREADY_PRESENT + 1))
            [ "$VERBOSE" = true ] && print_msg "$GREEN" "  ✓ $pkg already installed"
        fi
    done

    if [ ${#to_install[@]} -eq 0 ]; then
        print_msg "$GREEN" "All Ubuntu packages are already installed!"
        return 0
    fi

    if [ "$CHECK_ONLY" = true ]; then
        print_msg "$YELLOW" "Would install ${#to_install[@]} packages: ${to_install[*]}"
        return 0
    fi

    print_msg "$YELLOW" "Installing ${#to_install[@]} packages..."

    # Update package list
    print_msg "$BLUE" "Updating package list..."
    exec_cmd apt-get update

    # Install packages
    if exec_cmd apt-get install -y "${to_install[@]}"; then
        DEPS_INSTALLED=$((DEPS_INSTALLED + ${#to_install[@]}))
        print_msg "$GREEN" "Successfully installed ${#to_install[@]} packages"
    else
        DEPS_FAILED=$((DEPS_FAILED + ${#to_install[@]}))
        print_msg "$RED" "Failed to install some packages"
        return 1
    fi
}

# Install packages for Rocky Linux
install_rocky_deps() {
    print_msg "$BOLD" "Setting up Rocky Linux repositories..."

    # Enable required repositories
    if [ "$CHECK_ONLY" = false ]; then
        print_msg "$BLUE" "Enabling EPEL repository..."
        exec_cmd dnf install -y epel-release

        print_msg "$BLUE" "Enabling CRB repository..."
        exec_cmd dnf config-manager --set-enabled crb

        print_msg "$BLUE" "Installing RPM Fusion repositories..."
        exec_cmd dnf install -y --nogpgcheck \
            https://mirrors.rpmfusion.org/free/el/rpmfusion-free-release-9.noarch.rpm \
            https://mirrors.rpmfusion.org/nonfree/el/rpmfusion-nonfree-release-9.noarch.rpm
    fi

    # Separate required and optional packages
    local required_packages=(
        gcc
        gcc-c++
        git
        make
        ffmpeg-devel
        clang
        nasm
        opus-devel
        libvpx-devel
        x264-devel
        x265-devel
        libvorbis-devel
        lame-devel
        libass-devel
        pkgconfig
        openssl-devel
    )

    local to_install=()

    print_msg "$BOLD" "Checking Rocky Linux packages..."

    # Check required packages
    for pkg in "${required_packages[@]}"; do
        if [ "$FORCE_INSTALL" = true ] || ! check_dnf_package "$pkg"; then
            # Check if package is available in repos
            if dnf list available "$pkg" &>/dev/null || dnf list installed "$pkg" &>/dev/null; then
                to_install+=("$pkg")
                [ "$VERBOSE" = true ] && print_msg "$YELLOW" "  Will install: $pkg"
            else
                print_msg "$YELLOW" "  ⚠ Package not available: $pkg (skipping)"
                # Don't count unavailable packages as failures
            fi
        else
            DEPS_ALREADY_PRESENT=$((DEPS_ALREADY_PRESENT + 1))
            [ "$VERBOSE" = true ] && print_msg "$GREEN" "  ✓ $pkg already installed"
        fi
    done

    if [ ${#to_install[@]} -eq 0 ]; then
        print_msg "$GREEN" "All Rocky Linux packages are already installed!"
        return 0
    fi

    if [ "$CHECK_ONLY" = true ]; then
        print_msg "$YELLOW" "Would install ${#to_install[@]} packages: ${to_install[*]}"
        return 0
    fi

    # Install packages
    if [ ${#to_install[@]} -gt 0 ]; then
        print_msg "$YELLOW" "Installing ${#to_install[@]} packages..."
        if exec_cmd dnf install -y "${to_install[@]}"; then
            DEPS_INSTALLED=$((DEPS_INSTALLED + ${#to_install[@]}))
            print_msg "$GREEN" "Successfully installed ${#to_install[@]} packages"
        else
            DEPS_FAILED=$((DEPS_FAILED + ${#to_install[@]}))
            print_msg "$RED" "Failed to install some packages"
            return 1
        fi
    fi

    return 0
}

# Verify FFmpeg installation
verify_ffmpeg() {
    print_header "Verifying FFmpeg Installation"

    local ffmpeg_libs=(
        libavcodec
        libavformat
        libavutil
        libswscale
        libavfilter
        libavdevice
    )

    local all_found=true

    for lib in "${ffmpeg_libs[@]}"; do
        if pkg-config --exists "$lib" 2>/dev/null; then
            version=$(pkg-config --modversion "$lib" 2>/dev/null || echo "unknown")
            print_msg "$GREEN" "✓ $lib: $version"
        else
            print_msg "$RED" "✗ $lib not found"
            all_found=false
        fi
    done

    if [ "$all_found" = true ]; then
        print_msg "$GREEN" "All FFmpeg libraries are properly installed!"
        return 0
    else
        print_msg "$RED" "Some FFmpeg libraries are missing!"
        return 1
    fi
}

# Test compilation
test_compilation() {
    print_header "Testing Compilation"

    # Check for Rust
    if ! command -v cargo &> /dev/null; then
        print_msg "$YELLOW" "Rust is not installed. Skipping compilation test."
        print_msg "$BLUE" "To install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        return 0
    fi

    print_msg "$BLUE" "Rust version: $(rustc --version)"
    print_msg "$BLUE" "Cargo version: $(cargo --version)"

    # Try to build the project if we're in the right directory
    if [ -f "Cargo.toml" ]; then
        print_msg "$BLUE" "Building project to verify dependencies..."
        if cargo build --all-features > /dev/null 2>&1; then
            print_msg "$GREEN" "✓ Project builds successfully!"
            return 0
        else
            print_msg "$YELLOW" "⚠ Build failed. Run 'cargo build' for details."
            return 1
        fi
    else
        print_msg "$YELLOW" "Not in project directory, skipping build test"
    fi
}

# Main installation function
install_dependencies() {
    case "$PKG_MANAGER" in
        apt)
            install_ubuntu_deps
            ;;
        dnf)
            install_rocky_deps
            ;;
        *)
            print_msg "$RED" "Unknown package manager: $PKG_MANAGER"
            exit 1
            ;;
    esac
}

# Print summary
print_summary() {
    print_header "Installation Summary"

    if [ "$CHECK_ONLY" = true ]; then
        print_msg "$BLUE" "Check-only mode - no packages were installed"
    elif [ "$DRY_RUN" = true ]; then
        print_msg "$BLUE" "Dry-run mode - no packages were actually installed"
    else
        print_msg "$GREEN" "✓ Packages installed: $DEPS_INSTALLED"
        print_msg "$BLUE" "✓ Packages already present: $DEPS_ALREADY_PRESENT"
        if [ $DEPS_FAILED -gt 0 ]; then
            print_msg "$RED" "✗ Packages failed: $DEPS_FAILED"
        fi
    fi
}

# Main execution
main() {
    print_header "Video Storage Dependency Installer v$SCRIPT_VERSION"

    # Detect OS
    detect_os
    echo ""

    # Check sudo if needed
    if [ "$CHECK_ONLY" = false ] && [ "$DRY_RUN" = false ]; then
        check_sudo
    fi

    # Install dependencies
    if install_dependencies; then
        print_msg "$GREEN" "✓ Dependency installation completed successfully!"
    else
        print_msg "$RED" "✗ Some dependencies failed to install"
        DEPS_FAILED=1
    fi

    # Verify installation
    if [ "$CHECK_ONLY" = false ] && [ "$DRY_RUN" = false ]; then
        verify_ffmpeg
        test_compilation
    fi

    # Print summary
    print_summary

    # Exit with appropriate code
    if [ $DEPS_FAILED -gt 0 ]; then
        exit 1
    else
        exit 0
    fi
}

# Parse arguments
parse_args "$@"

# Run main
main
