#!/usr/bin/env python3
"""
This script processes directories and files, generates a tree structure, and
concatenates the contents of specified files into an output file. It supports
wildcard patterns, respects ignore patterns from .gitignore and additional
--ignore arguments using the `pathspec` library, and outputs statistics about
the processed data.

Key Features:
- Generates a unified tree structure using Python, representing both
directories and specific files.
- Applies ignore patterns from .gitignore and additional --ignore arguments
using the `pathspec` library.
- Includes debug logging that can be enabled with the `--debug` argument.
- Uses `--path` to specify the working directory for processing.
- Displays the tree structure on the command line.
"""
import argparse
import glob
import logging
import os
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import Dict, List

import pathspec


def setup_logging(debug: bool = False) -> logging.Logger:
    """Set up logging configuration."""
    level = logging.DEBUG if debug else logging.INFO
    logging.basicConfig(level=level, format="%(message)s")
    return logging.getLogger(__name__)


def load_gitignore_patterns(gitignore_path: Path) -> List[str]:
    """Load ignore patterns from a .gitignore file."""
    if not gitignore_path.exists():
        return []
    with gitignore_path.open("r", encoding="utf-8") as f:
        lines = f.readlines()
    patterns = [
        line.strip()
        for line in lines
        if line.strip() and not line.strip().startswith("#")
    ]
    return patterns


def compile_ignore_spec(
    gitignore_patterns: List[str], cli_ignore_patterns: List[str]
) -> pathspec.PathSpec:
    """Compile ignore patterns using pathspec."""
    combined_patterns = gitignore_patterns + cli_ignore_patterns
    return pathspec.PathSpec.from_lines("gitwildmatch", combined_patterns)


def expand_globs(patterns: List[str]) -> List[Path]:
    """Expand glob patterns to paths."""
    expanded = []
    for pattern in patterns:
        # Use recursive glob if pattern contains **
        if "**" in pattern:
            matched = glob.glob(pattern, recursive=True)
        else:
            matched = glob.glob(pattern)
        expanded.extend(
            [Path(p).resolve() for p in matched if Path(p).exists()]
        )
    return expanded


def collect_files(
    dirs: List[Path],
    files: List[Path],
    spec: pathspec.PathSpec,
    logger: logging.Logger,
) -> List[Path]:
    """
    Collect files from directories and specific files, applying ignore
    patterns and skipping binary files.
    """
    collected = set()
    
    # Add data/ directory to ignored paths
    data_dir = Path('data')

    # Process directories
    for dir_path in dirs:
        if not dir_path.is_dir():
            logger.warning(f"Specified path is not a directory: {dir_path}")
            continue
        for root, _, filenames in os.walk(dir_path):
            root_path = Path(root).resolve()
            for filename in filenames:
                file_path = root_path / filename
                try:
                    relative_path = file_path.relative_to(Path.cwd())
                except ValueError:
                    # If file is not under current working directory, skip
                    logger.warning(
                        f"File {file_path} is not under the current working "
                        "directory."
                    )
                    continue
                # Skip files in data directory
                if data_dir in file_path.parents:
                    logger.debug(f"Skipping file in data directory: {relative_path}")
                    continue
                
                if not spec.match_file(str(relative_path)):
                    if not is_binary_file(file_path):
                        collected.add(file_path)
                    else:
                        logger.debug(f"Skipping binary file: {relative_path}")

    # Process specific files
    for file_path in files:
        if not file_path.is_file():
            logger.warning(
                "Specified file does not exist or is not a regular file: "
                f"{file_path}"
            )
            continue
        try:
            relative_path = file_path.relative_to(Path.cwd())
        except ValueError:
            relative_path = file_path
        # Skip files in data directory
        if data_dir in file_path.parents:
            logger.debug(f"Skipping file in data directory: {relative_path}")
            continue
            
        if not spec.match_file(str(relative_path)):
            if not is_binary_file(file_path):
                collected.add(file_path.resolve())
            else:
                logger.debug(f"Skipping binary file: {relative_path}")

    return sorted(collected)


def build_tree(paths: List[Path], root: Path) -> Dict:
    """Build a nested dictionary representing the tree structure."""
    tree = {".": {}}
    for path in paths:
        try:
            relative_path = path.relative_to(root)
        except ValueError:
            # If path is not relative to root, skip it
            continue
        parts = relative_path.parts
        current_level = tree["."]
        for part in parts[:-1]:
            current_level = current_level.setdefault(part, {})
        current_level[parts[-1]] = None  # Files are leaves
    return tree


def print_tree(tree: Dict, prefix: str = "") -> str:
    """Generate a string representation of the tree structure."""
    lines = []
    entries = sorted(tree.keys())
    for idx, key in enumerate(entries):
        connector = "└── " if idx == len(entries) - 1 else "├── "
        lines.append(f"{prefix}{connector}{key}")
        if isinstance(tree[key], dict):
            extension = "    " if idx == len(entries) - 1 else "│   "
            lines.append(print_tree(tree[key], prefix + extension))
    return "\n".join(lines)


def is_binary_file(file_path: Path) -> bool:
    """Check if a file is binary by reading its first chunk."""
    try:
        chunk_size = 8192
        with open(file_path, 'rb') as f:
            chunk = f.read(chunk_size)
            return b'\0' in chunk  # Binary files typically contain null bytes
    except Exception:
        return True  # Assume binary if we can't read the file

def format_file_size(size: int) -> str:
    """Format file size in bytes to a human-readable form."""
    units = ["bytes", "KB", "MB", "GB", "TB"]
    index = 0
    while size >= 1024 and index < len(units) - 1:
        size /= 1024.0
        index += 1
    return (
        f"{size:.2f} {units[index]}"
        if index > 0
        else f"{int(size)} {units[index]}"
    )


def generate_output_filename(
    git_root: Path, provided_output: str = None
) -> Path:
    """
    Generate the output file name based on the provided argument or default
    pattern.

    Args:
        git_root (Path): Git repository root path.
        provided_output (str, optional): User-specified output file path.

    Returns:
        Path: Path to the output file.
    """
    if provided_output:
        return Path(provided_output).resolve()
    else:
        current_datetime = datetime.now().strftime("%Y%m%d%H%M%S")
        output_dir = git_root / "data" if git_root.exists() else Path.cwd()
        output_dir.mkdir(parents=True, exist_ok=True)
        return output_dir / f"workflows_o1_preview_{current_datetime}.txt"


def main():
    """
    Main function to parse arguments and execute processing.
    """
    parser = argparse.ArgumentParser(
        description="Process directories and files, generating a concatenated "
        "output."
    )
    parser.add_argument(
        "--dir", nargs="+", help="Directory to recursively process", default=[]
    )
    parser.add_argument(
        "--file", nargs="+", help="Specific files to include", default=[]
    )
    parser.add_argument(
        "--ignore",
        nargs="+",
        help="Pattern to ignore (can be used multiple times)",
        default=[],
    )
    parser.add_argument(
        "--output",
        help="Output file (default: workflows_o1_preview_YYYYMMDDHHMMSS.txt in"
        " the root directory)",
    )
    parser.add_argument(
        "--path", help="Path to run commands from (default: current directory)"
    )
    parser.add_argument(
        "--debug", action="store_true", help="Enable debug logging"
    )
    args = parser.parse_args()

    # Set up logging
    logger = setup_logging(debug=args.debug)

    # Change working directory if --path is provided
    if args.path:
        try:
            os.chdir(args.path)
            logger.debug(f"Changed working directory to {args.path}")
        except Exception as e:
            logger.error(f"Failed to change directory to {args.path}: {e}")
            sys.exit(1)

    # Determine Git repository root
    try:
        git_root_str = (
            subprocess.check_output(
                ["git", "rev-parse", "--show-toplevel"],
                stderr=subprocess.STDOUT,
            )
            .decode()
            .strip()
        )
        git_root = Path(git_root_str).resolve()
    except subprocess.CalledProcessError:
        git_root = Path.cwd()
        logger.warning(
            "Not inside a Git repository. Using current directory as root."
        )

    # Load .gitignore patterns
    gitignore_path = git_root / ".gitignore"
    gitignore_patterns = load_gitignore_patterns(gitignore_path)
    logger.debug(f"Patterns from .gitignore: {gitignore_patterns}")

    # Combine with additional ignore patterns from --ignore
    cli_ignore_patterns = args.ignore
    logger.debug(
        f"Additional ignore patterns from --ignore: {cli_ignore_patterns}"
    )

    # Compile ignore specifications
    spec = compile_ignore_spec(gitignore_patterns, cli_ignore_patterns)
    logger.debug("Compiled ignore spec.")

    # Expand and resolve directories and files
    dirs = expand_globs(args.dir) if args.dir else []
    files = expand_globs(args.file) if args.file else []
    logger.debug(f"Directories to process: {dirs}")
    logger.debug(f"Files to include: {files}")

    # Collect files applying ignore patterns
    collected_files = collect_files(dirs, files, spec, logger)
    logger.debug(
        f"Collected {len(collected_files)} files after applying ignore "
        "patterns."
    )

    # If no files to process, exit
    if not collected_files:
        logger.info("No files to process after applying ignore patterns.")
        sys.exit(0)

    # Build tree structure
    tree = build_tree(collected_files, Path.cwd())
    tree_str = print_tree(tree)
    logger.debug("Built tree structure.")

    # Display the tree structure on the command line
    logger.info("Tree structure:")
    logger.info(tree_str + "\n")

    # Generate output file name
    output_file = generate_output_filename(git_root, args.output)
    logger.info(f"Output file: {output_file}\n")

    # Ensure the output directory exists
    output_file.parent.mkdir(parents=True, exist_ok=True)
    logger.debug(f"Ensured that the directory {output_file.parent} exists.")

    # Write the tree structure and concatenate file contents
    try:
        with output_file.open("w", encoding="utf-8") as f_out:
            f_out.write("Tree structure:\n")
            f_out.write(tree_str)
            f_out.write("\n\n")

            # Concatenate files
            for file_path in collected_files:
                try:
                    relative_file_path = file_path.relative_to(Path.cwd())
                except ValueError:
                    # If file is not relative to cwd, use absolute path
                    relative_file_path = file_path
                if is_binary_file(file_path):
                    logger.warning(f"Skipping binary file: {relative_file_path}")
                    continue
                    
                f_out.write(f"# {relative_file_path}\n")
                try:
                    with file_path.open(
                        "r", encoding="utf-8", errors="replace"
                    ) as f_in:
                        content = f_in.read()
                        f_out.write(content)
                    f_out.write("\n")
                    logger.debug(f"Appended content from {file_path}")
                except Exception as e:
                    logger.error(f"Failed to read file {relative_file_path}: {e}")
        logger.info("Successfully created the output file.")
    except Exception as e:
        logger.error(f"Failed to create the output file: {e}")
        sys.exit(1)

    # Log statistics
    total_files = len(collected_files)
    try:
        total_size_bytes = output_file.stat().st_size
        total_size_str = format_file_size(total_size_bytes)
    except Exception:
        total_size_str = "Unknown"

    logger.info(f"Total files included: {total_files}")
    logger.info(f"Total size of output file: {total_size_str}")


if __name__ == "__main__":
    main()
