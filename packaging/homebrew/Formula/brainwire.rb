require "json"
require "shellwords"

class Brainwire < Formula
  desc "Graph database for connected context and agent memory"
  homepage "https://github.com/allandefraga/brainwire"
  url "https://github.com/allandefraga/brainwire/archive/refs/tags/v0.24.0-beta.2.tar.gz"
  sha256 "1c7bb8490f0d74f76f3a9cce9d1da0a0e8a81e10b00af9f95f748a85fc150157"
  license "Apache-2.0"

  depends_on "pkgconf" => :build
  depends_on "rust" => :build
  depends_on :macos

  def install
    clang = Pathname(Utils.safe_popen_read("/usr/bin/xcrun", "--find", "clang").strip)
    ENV["LIBCLANG_PATH"] = (clang.dirname.parent/"lib").to_s
    ENV.prepend_path "PATH", formula_opt_bin("rust")
    # RocksDB's C++ compilation uses substantial memory per job.
    jobs = [ENV.make_jobs, 2].min.to_s
    system "cargo", "install", "--locked", "--jobs", jobs, "--root", prefix,
           "--path", "crates/server", "--bin", "brainwire"
    pkgshare.install "examples/company_brain/ontology.json" => "company-ontology.json"
  end

  test do
    ENV["BRAINWIRE_MEMORY_CACHE"] = "64MiB"
    ENV["BRAINWIRE_MEMORY_MEMTABLES"] = "8MiB"
    assert_equal "brainwire #{version}\n", shell_output("#{bin}/brainwire --version")
    database = testpath/"graph"
    write = "CREATE (:Project {entity_id: 'p1', name: 'Release'})"
    system bin/"brainwire", "cypher", "--database", database, "--ontology",
           pkgshare/"company-ontology.json", "--query", write

    query = "MATCH (p:Project {entity_id: 'p1'}) RETURN p.name"
    argv = [bin/"brainwire", "cypher", "--database", database, "--query", query]
    assert_equal [["Release"]], JSON.parse(shell_output(Shellwords.join(argv))).fetch("rows")
  end
end
