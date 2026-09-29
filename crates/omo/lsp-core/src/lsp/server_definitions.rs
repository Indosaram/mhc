//! Tables ported verbatim from TS `server-definitions.ts` (declaration order preserved).

pub struct BuiltinServer {
    pub id: &'static str,
    pub command: &'static [&'static str],
    pub extensions: &'static [&'static str],
}

impl BuiltinServer {
    pub fn command_vec(&self) -> Vec<String> {
        self.command.iter().map(|s| (*s).to_string()).collect()
    }

    pub fn extensions_vec(&self) -> Vec<String> {
        self.extensions.iter().map(|s| (*s).to_string()).collect()
    }
}

pub const LSP_INSTALL_HINTS: &[(&str, &str)] = &[
    (
        "typescript",
        "npm install -g typescript-language-server typescript",
    ),
    ("deno", "Install Deno from https://deno.land"),
    ("vue", "npm install -g @vue/language-server"),
    ("eslint", "npm install -g vscode-langservers-extracted"),
    ("oxlint", "npm install -g oxlint"),
    ("biome", "npm install -g @biomejs/biome"),
    ("gopls", "go install golang.org/x/tools/gopls@latest"),
    ("ruby-lsp", "gem install ruby-lsp"),
    ("basedpyright", "pip install basedpyright"),
    ("pyright", "pip install pyright"),
    ("ty", "pip install ty"),
    ("ruff", "pip install ruff"),
    ("elixir-ls", "See https://github.com/elixir-lsp/elixir-ls"),
    ("zls", "See https://github.com/zigtools/zls"),
    ("csharp", "dotnet tool install -g csharp-ls"),
    ("fsharp", "dotnet tool install -g fsautocomplete"),
    ("sourcekit-lsp", "Included with Xcode or Swift toolchain"),
    (
        "rust",
        "Install rust-analyzer and ensure it is in PATH. If using rustup: rustup component add rust-analyzer. If rust-analyzer exits while loading rust-src: rustup component remove rust-src && rustup component add rust-src.",
    ),
    ("clangd", "See https://clangd.llvm.org/installation"),
    ("svelte", "npm install -g svelte-language-server"),
    ("astro", "npm install -g @astrojs/language-server"),
    ("bash-ls", "npm install -g bash-language-server"),
    (
        "jdtls",
        "See https://github.com/eclipse-jdtls/eclipse.jdt.ls",
    ),
    ("yaml-ls", "npm install -g yaml-language-server"),
    ("lua-ls", "See https://github.com/LuaLS/lua-language-server"),
    ("php", "npm install -g intelephense"),
    ("dart", "Included with Dart SDK"),
    (
        "terraform-ls",
        "See https://github.com/hashicorp/terraform-ls",
    ),
    ("terraform", "See https://github.com/hashicorp/terraform-ls"),
    ("prisma", "npm install -g prisma"),
    ("ocaml-lsp", "opam install ocaml-lsp-server"),
    ("texlab", "See https://github.com/latex-lsp/texlab"),
    (
        "dockerfile",
        "npm install -g dockerfile-language-server-nodejs",
    ),
    ("gleam", "See https://gleam.run/getting-started/installing/"),
    ("clojure-lsp", "See https://clojure-lsp.io/installation/"),
    ("nixd", "nix profile install nixpkgs#nixd"),
    ("tinymist", "See https://github.com/Myriad-Dreamin/tinymist"),
    ("haskell-language-server", "ghcup install hls"),
    ("bash", "npm install -g bash-language-server"),
    ("kotlin-ls", "See https://github.com/Kotlin/kotlin-lsp"),
    (
        "julials",
        "julia -e 'using Pkg; Pkg.add(\"LanguageServer\")'",
    ),
    (
        "razor",
        "Razor runs through the Roslyn language server (cohosting). Install: dotnet tool install -g roslyn-language-server --prerelease (requires v5.8.0+). See https://github.com/dotnet/razor",
    ),
];

pub const BUILTIN_SERVERS: &[BuiltinServer] = &[
    BuiltinServer {
        id: "typescript",
        command: &["typescript-language-server", "--stdio"],
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"],
    },
    BuiltinServer {
        id: "deno",
        command: &["deno", "lsp"],
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mjs"],
    },
    BuiltinServer {
        id: "vue",
        command: &["vue-language-server", "--stdio"],
        extensions: &[".vue"],
    },
    BuiltinServer {
        id: "eslint",
        command: &["vscode-eslint-language-server", "--stdio"],
        extensions: &[
            ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".vue",
        ],
    },
    BuiltinServer {
        id: "oxlint",
        command: &["oxlint", "--lsp"],
        extensions: &[
            ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".vue", ".astro",
            ".svelte",
        ],
    },
    BuiltinServer {
        id: "biome",
        command: &["biome", "lsp-proxy", "--stdio"],
        extensions: &[
            ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".json", ".jsonc",
            ".vue", ".astro", ".svelte", ".css", ".graphql", ".gql", ".html",
        ],
    },
    BuiltinServer {
        id: "gopls",
        command: &["gopls"],
        extensions: &[".go"],
    },
    BuiltinServer {
        id: "ruby-lsp",
        command: &["rubocop", "--lsp"],
        extensions: &[".rb", ".rake", ".gemspec", ".ru"],
    },
    BuiltinServer {
        id: "basedpyright",
        command: &["basedpyright-langserver", "--stdio"],
        extensions: &[".py", ".pyi"],
    },
    BuiltinServer {
        id: "pyright",
        command: &["pyright-langserver", "--stdio"],
        extensions: &[".py", ".pyi"],
    },
    BuiltinServer {
        id: "ty",
        command: &["ty", "server"],
        extensions: &[".py", ".pyi"],
    },
    BuiltinServer {
        id: "ruff",
        command: &["ruff", "server"],
        extensions: &[".py", ".pyi"],
    },
    BuiltinServer {
        id: "elixir-ls",
        command: &["elixir-ls"],
        extensions: &[".ex", ".exs"],
    },
    BuiltinServer {
        id: "zls",
        command: &["zls"],
        extensions: &[".zig", ".zon"],
    },
    BuiltinServer {
        id: "csharp",
        command: &["csharp-ls"],
        extensions: &[".cs"],
    },
    BuiltinServer {
        id: "fsharp",
        command: &["fsautocomplete"],
        extensions: &[".fs", ".fsi", ".fsx", ".fsscript"],
    },
    BuiltinServer {
        id: "sourcekit-lsp",
        command: &["sourcekit-lsp"],
        extensions: &[".swift", ".m", ".mm"],
    },
    BuiltinServer {
        id: "rust",
        command: &["rust-analyzer"],
        extensions: &[".rs"],
    },
    BuiltinServer {
        id: "clangd",
        command: &["clangd", "--background-index", "--clang-tidy"],
        extensions: &[
            ".c", ".cpp", ".cc", ".cxx", ".c++", ".h", ".hpp", ".hh", ".hxx", ".h++",
        ],
    },
    BuiltinServer {
        id: "svelte",
        command: &["svelteserver", "--stdio"],
        extensions: &[".svelte"],
    },
    BuiltinServer {
        id: "astro",
        command: &["astro-ls", "--stdio"],
        extensions: &[".astro"],
    },
    BuiltinServer {
        id: "bash",
        command: &["bash-language-server", "start"],
        extensions: &[".sh", ".bash", ".zsh", ".ksh"],
    },
    BuiltinServer {
        id: "bash-ls",
        command: &["bash-language-server", "start"],
        extensions: &[".sh", ".bash", ".zsh", ".ksh"],
    },
    BuiltinServer {
        id: "jdtls",
        command: &["jdtls"],
        extensions: &[".java"],
    },
    BuiltinServer {
        id: "yaml-ls",
        command: &["yaml-language-server", "--stdio"],
        extensions: &[".yaml", ".yml"],
    },
    BuiltinServer {
        id: "lua-ls",
        command: &["lua-language-server"],
        extensions: &[".lua"],
    },
    BuiltinServer {
        id: "php",
        command: &["intelephense", "--stdio"],
        extensions: &[".php"],
    },
    BuiltinServer {
        id: "dart",
        command: &["dart", "language-server", "--lsp"],
        extensions: &[".dart"],
    },
    BuiltinServer {
        id: "terraform",
        command: &["terraform-ls", "serve"],
        extensions: &[".tf", ".tfvars"],
    },
    BuiltinServer {
        id: "terraform-ls",
        command: &["terraform-ls", "serve"],
        extensions: &[".tf", ".tfvars"],
    },
    BuiltinServer {
        id: "prisma",
        command: &["prisma", "language-server"],
        extensions: &[".prisma"],
    },
    BuiltinServer {
        id: "ocaml-lsp",
        command: &["ocamllsp"],
        extensions: &[".ml", ".mli"],
    },
    BuiltinServer {
        id: "texlab",
        command: &["texlab"],
        extensions: &[".tex", ".bib"],
    },
    BuiltinServer {
        id: "dockerfile",
        command: &["docker-langserver", "--stdio"],
        extensions: &[".dockerfile"],
    },
    BuiltinServer {
        id: "gleam",
        command: &["gleam", "lsp"],
        extensions: &[".gleam"],
    },
    BuiltinServer {
        id: "clojure-lsp",
        command: &["clojure-lsp", "listen"],
        extensions: &[".clj", ".cljs", ".cljc", ".edn"],
    },
    BuiltinServer {
        id: "nixd",
        command: &["nixd"],
        extensions: &[".nix"],
    },
    BuiltinServer {
        id: "tinymist",
        command: &["tinymist"],
        extensions: &[".typ", ".typc"],
    },
    BuiltinServer {
        id: "haskell-language-server",
        command: &["haskell-language-server-wrapper", "--lsp"],
        extensions: &[".hs", ".lhs"],
    },
    BuiltinServer {
        id: "kotlin-ls",
        command: &["kotlin-lsp", "--stdio"],
        extensions: &[".kt", ".kts"],
    },
    BuiltinServer {
        id: "julials",
        command: &[
            "julia",
            "--startup-file=no",
            "--history-file=no",
            "-e",
            "using LanguageServer; runserver()",
        ],
        extensions: &[".jl"],
    },
    BuiltinServer {
        id: "razor",
        command: &["roslyn-language-server", "--stdio"],
        extensions: &[".razor", ".cshtml"],
    },
];

pub const AUTO_INSTALLABLE_SERVERS: &[(&str, &[&str])] = &[
    (
        "typescript",
        &[
            "npm",
            "install",
            "-g",
            "typescript-language-server",
            "typescript",
        ],
    ),
    ("vue", &["npm", "install", "-g", "@vue/language-server"]),
    (
        "eslint",
        &["npm", "install", "-g", "vscode-langservers-extracted"],
    ),
    ("oxlint", &["npm", "install", "-g", "oxlint"]),
    ("biome", &["npm", "install", "-g", "@biomejs/biome"]),
    (
        "svelte",
        &["npm", "install", "-g", "svelte-language-server"],
    ),
    (
        "astro",
        &["npm", "install", "-g", "@astrojs/language-server"],
    ),
    ("bash-ls", &["npm", "install", "-g", "bash-language-server"]),
    ("bash", &["npm", "install", "-g", "bash-language-server"]),
    ("yaml-ls", &["npm", "install", "-g", "yaml-language-server"]),
    ("php", &["npm", "install", "-g", "intelephense"]),
    ("prisma", &["npm", "install", "-g", "prisma"]),
    (
        "dockerfile",
        &["npm", "install", "-g", "dockerfile-language-server-nodejs"],
    ),
    (
        "gopls",
        &["go", "install", "golang.org/x/tools/gopls@latest"],
    ),
    ("pyright", &["pip", "install", "pyright"]),
    ("basedpyright", &["pip", "install", "basedpyright"]),
    ("ruff", &["pip", "install", "ruff"]),
    ("ty", &["pip", "install", "ty"]),
    ("ruby-lsp", &["gem", "install", "ruby-lsp"]),
    ("ocaml-lsp", &["opam", "install", "ocaml-lsp-server"]),
    (
        "julials",
        &["julia", "-e", "using Pkg; Pkg.add(\"LanguageServer\")"],
    ),
];

pub fn builtin_server(id: &str) -> Option<&'static BuiltinServer> {
    BUILTIN_SERVERS.iter().find(|server| server.id == id)
}

pub fn install_hint(id: &str) -> Option<&'static str> {
    LSP_INSTALL_HINTS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, hint)| *hint)
}

pub fn auto_install_command(id: &str) -> Option<&'static [&'static str]> {
    AUTO_INSTALLABLE_SERVERS
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, command)| *command)
}
