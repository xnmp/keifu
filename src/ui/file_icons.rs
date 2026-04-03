//! Nerd Font file icons by extension and filename.

use ratatui::style::Color;
use std::path::Path;

pub struct FileIcon {
    pub icon: &'static str,
    pub color: Color,
}

const DEFAULT: FileIcon = FileIcon { icon: "\u{f15b}", color: Color::Gray }; //

pub fn file_icon(path: &Path) -> FileIcon {
    // Check exact filename first
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Some(icon) = by_filename(name) {
            return icon;
        }
    }

    // Then by extension
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if let Some(icon) = by_extension(ext) {
            return icon;
        }
    }

    DEFAULT
}

fn by_filename(name: &str) -> Option<FileIcon> {
    Some(match name.to_lowercase().as_str() {
        "makefile" | "gnumakefile" => FileIcon { icon: "\u{e779}", color: Color::Gray },           //
        "dockerfile" | "containerfile" => FileIcon { icon: "\u{f308}", color: Color::Blue },       //
        "docker-compose.yml" | "docker-compose.yaml" => FileIcon { icon: "\u{f308}", color: Color::Blue },
        ".gitignore" | ".gitattributes" | ".gitmodules" => FileIcon { icon: "\u{e702}", color: Color::Red }, //
        ".env" | ".env.local" | ".env.example" => FileIcon { icon: "\u{f462}", color: Color::Yellow },
        "cargo.toml" | "cargo.lock" => FileIcon { icon: "\u{e7a8}", color: Color::Yellow },        //
        "package.json" | "package-lock.json" => FileIcon { icon: "\u{e718}", color: Color::Green }, //
        "tsconfig.json" => FileIcon { icon: "\u{e628}", color: Color::Blue },
        "readme.md" | "readme" => FileIcon { icon: "\u{f48a}", color: Color::Blue },               //
        "license" | "license.md" | "license.txt" => FileIcon { icon: "\u{f718}", color: Color::Yellow },
        "flake.nix" => FileIcon { icon: "\u{f313}", color: Color::LightCyan },                     //
        _ => return None,
    })
}

fn by_extension(ext: &str) -> Option<FileIcon> {
    Some(match ext.to_lowercase().as_str() {
        // Rust
        "rs" => FileIcon { icon: "\u{e7a8}", color: Color::Rgb(222, 165, 80) },    //

        // Python
        "py" | "pyi" | "pyw" => FileIcon { icon: "\u{e73c}", color: Color::Rgb(55, 118, 171) }, //
        "ipynb" => FileIcon { icon: "\u{e678}", color: Color::Rgb(227, 137, 52) },

        // JavaScript / TypeScript
        "js" | "mjs" | "cjs" => FileIcon { icon: "\u{e74e}", color: Color::Yellow },  //
        "jsx" => FileIcon { icon: "\u{e7ba}", color: Color::Cyan },                   //
        "ts" | "mts" | "cts" => FileIcon { icon: "\u{e628}", color: Color::Blue },     //
        "tsx" => FileIcon { icon: "\u{e7ba}", color: Color::Blue },
        "vue" => FileIcon { icon: "\u{e6a0}", color: Color::Green },
        "svelte" => FileIcon { icon: "\u{e697}", color: Color::Red },

        // Web
        "html" | "htm" => FileIcon { icon: "\u{e736}", color: Color::Rgb(228, 77, 38) }, //
        "css" => FileIcon { icon: "\u{e749}", color: Color::Blue },                       //
        "scss" | "sass" => FileIcon { icon: "\u{e603}", color: Color::Magenta },
        "less" => FileIcon { icon: "\u{e758}", color: Color::Blue },

        // Data / Config
        "json" | "jsonc" => FileIcon { icon: "\u{e60b}", color: Color::Yellow },   //
        "yaml" | "yml" => FileIcon { icon: "\u{e6a8}", color: Color::Red },
        "toml" => FileIcon { icon: "\u{e6b2}", color: Color::Gray },
        "xml" | "xsl" | "xslt" => FileIcon { icon: "\u{e619}", color: Color::Rgb(228, 77, 38) },
        "csv" => FileIcon { icon: "\u{f1c3}", color: Color::Green },
        "sql" => FileIcon { icon: "\u{e706}", color: Color::LightBlue },

        // Shell
        "sh" | "bash" | "zsh" | "fish" => FileIcon { icon: "\u{e795}", color: Color::Green }, //
        "ps1" | "psm1" => FileIcon { icon: "\u{ebc7}", color: Color::Blue },

        // C / C++
        "c" => FileIcon { icon: "\u{e61e}", color: Color::Blue },      //
        "h" => FileIcon { icon: "\u{e61e}", color: Color::Magenta },
        "cpp" | "cc" | "cxx" => FileIcon { icon: "\u{e61d}", color: Color::Blue }, //
        "hpp" | "hh" | "hxx" => FileIcon { icon: "\u{e61d}", color: Color::Magenta },

        // Go
        "go" => FileIcon { icon: "\u{e627}", color: Color::Cyan },     //

        // Java / JVM
        "java" => FileIcon { icon: "\u{e738}", color: Color::Red },    //
        "kt" | "kts" => FileIcon { icon: "\u{e634}", color: Color::Magenta },
        "scala" | "sc" => FileIcon { icon: "\u{e737}", color: Color::Red },
        "gradle" => FileIcon { icon: "\u{e660}", color: Color::LightBlue },

        // Ruby
        "rb" | "gemspec" => FileIcon { icon: "\u{e739}", color: Color::Red },  //
        "erb" => FileIcon { icon: "\u{e739}", color: Color::Red },

        // PHP
        "php" => FileIcon { icon: "\u{e73d}", color: Color::Magenta }, //

        // Swift / Objective-C
        "swift" => FileIcon { icon: "\u{e755}", color: Color::Rgb(240, 81, 56) },
        "m" | "mm" => FileIcon { icon: "\u{e61e}", color: Color::Blue },

        // Elixir / Erlang
        "ex" | "exs" => FileIcon { icon: "\u{e62d}", color: Color::Magenta },
        "erl" | "hrl" => FileIcon { icon: "\u{e7b1}", color: Color::Red },

        // Haskell
        "hs" | "lhs" => FileIcon { icon: "\u{e777}", color: Color::Magenta },

        // Lua
        "lua" => FileIcon { icon: "\u{e620}", color: Color::Blue },

        // Nix
        "nix" => FileIcon { icon: "\u{f313}", color: Color::LightCyan },

        // Markdown / Docs
        "md" | "mdx" => FileIcon { icon: "\u{e73e}", color: Color::Blue },     //
        "txt" | "text" => FileIcon { icon: "\u{f15c}", color: Color::Gray },
        "rst" => FileIcon { icon: "\u{f718}", color: Color::Gray },
        "org" => FileIcon { icon: "\u{e633}", color: Color::Cyan },
        "tex" | "latex" => FileIcon { icon: "\u{e69b}", color: Color::Green },
        "pdf" => FileIcon { icon: "\u{f1c1}", color: Color::Red },

        // Images
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "avif" => {
            FileIcon { icon: "\u{f1c5}", color: Color::Magenta }
        }
        "svg" => FileIcon { icon: "\u{f1c5}", color: Color::Yellow },

        // Fonts
        "ttf" | "otf" | "woff" | "woff2" => FileIcon { icon: "\u{f031}", color: Color::Gray },

        // Archives
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" => {
            FileIcon { icon: "\u{f1c6}", color: Color::Yellow }
        }

        // Binary / Compiled
        "wasm" => FileIcon { icon: "\u{e6a1}", color: Color::Magenta },
        "so" | "dylib" | "dll" => FileIcon { icon: "\u{f471}", color: Color::Gray },

        // Lock files
        "lock" => FileIcon { icon: "\u{f023}", color: Color::Gray },

        // Docker
        "dockerignore" => FileIcon { icon: "\u{f308}", color: Color::Blue },

        _ => return None,
    })
}
