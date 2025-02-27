// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// todo(dsherret): move the integration-like tests to this file because it
// helps ensure we're testing the public API and ensures we export types
// out of deno_graph that should be public

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::anyhow;
use deno_ast::ModuleSpecifier;
use deno_graph::source::CacheSetting;
use deno_graph::source::LoadFuture;
use deno_graph::source::LoadResponse;
use deno_graph::source::MemoryFileSystem;
use deno_graph::source::MemoryLoader;
use deno_graph::source::NpmResolver;
use deno_graph::source::Source;
use deno_graph::source::UnknownBuiltInNodeModuleError;
use deno_graph::BuildOptions;
use deno_graph::GraphKind;
use deno_graph::ModuleGraph;
use deno_graph::NpmPackageReqResolution;
use deno_graph::Range;
use deno_semver::package::PackageNv;
use deno_semver::package::PackageReq;
use deno_semver::Version;
use futures::future::LocalBoxFuture;
use pretty_assertions::assert_eq;
use url::Url;

use crate::helpers::get_specs_in_dir;
use crate::helpers::TestBuilder;

mod helpers;

#[tokio::test]
async fn test_graph_specs() {
  for (test_file_path, spec) in
    get_specs_in_dir(&PathBuf::from("./tests/specs/graph"))
  {
    eprintln!("Running {}", test_file_path.display());
    let mut builder = TestBuilder::new();
    builder.with_loader(|loader| {
      for file in &spec.files {
        let source = Source::Module {
          specifier: file.url().to_string(),
          maybe_headers: Some(file.headers.clone().into_iter().collect()),
          content: file.text.clone(),
        };
        if file.is_cache() {
          loader.cache.add_source(file.url(), source);
        } else {
          loader.remote.add_source(file.url(), source);
        }
      }
    });
    builder.workspace_members(spec.workspace_members.clone());

    let result = builder.build().await;
    let update_var = std::env::var("UPDATE");
    let mut output_text = serde_json::to_string_pretty(&result.graph).unwrap();
    output_text.push('\n');
    let diagnostics = result
      .diagnostics
      .iter()
      .map(|d| serde_json::to_value(d.to_string()).unwrap())
      .collect::<Vec<_>>();
    let spec = if update_var.as_ref().map(|v| v.as_str()) == Ok("1") {
      let mut spec = spec;
      spec.output_file.text = output_text.clone();
      spec.diagnostics = diagnostics.clone();
      std::fs::write(&test_file_path, spec.emit()).unwrap();
      spec
    } else {
      spec
    };
    assert_eq!(
      output_text,
      spec.output_file.text,
      "Should be same for {}",
      test_file_path.display()
    );
    assert_eq!(
      diagnostics,
      spec.diagnostics,
      "Should be same for {}",
      test_file_path.display()
    );
  }
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn test_symbols_specs() {
  for (test_file_path, spec) in
    get_specs_in_dir(&PathBuf::from("./tests/specs/symbols"))
  {
    eprintln!("Running {}", test_file_path.display());
    let mut builder = TestBuilder::new();

    if spec.files.iter().any(|f| f.specifier == "mod.js") {
      // this is for the TypesEntrypoint test
      builder.entry_point("file:///mod.js");
      builder.entry_point_types("file:///mod.d.ts");
    }

    builder.with_loader(|loader| {
      for file in &spec.files {
        let source = Source::Module {
          specifier: file.url().to_string(),
          maybe_headers: Some(file.headers.clone().into_iter().collect()),
          content: file.text.clone(),
        };
        if file.is_cache() {
          loader.cache.add_source(file.url(), source);
        } else {
          loader.remote.add_source(file.url(), source);
        }
      }
    });

    let result = builder.symbols().await.unwrap();
    let update_var = std::env::var("UPDATE");
    let diagnostics = result
      .diagnostics
      .iter()
      .map(|d| serde_json::to_value(d.clone()).unwrap())
      .collect::<Vec<_>>();
    let spec = if update_var.as_ref().map(|v| v.as_str()) == Ok("1") {
      let mut spec = spec;
      spec.output_file.text = result.output.clone();
      spec.diagnostics = diagnostics.clone();
      std::fs::write(&test_file_path, spec.emit()).unwrap();
      spec
    } else {
      spec
    };
    assert_eq!(
      result.output,
      spec.output_file.text,
      "Should be same for {}",
      test_file_path.display()
    );
    assert_eq!(
      diagnostics,
      spec.diagnostics,
      "Should be same for {}",
      test_file_path.display()
    );
  }
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn test_symbols_dep_definition() {
  use deno_graph::symbols::ResolvedSymbolDepEntry;

  let result = TestBuilder::new()
    .with_loader(|loader| {
      loader.remote.add_source_with_text(
        "file:///mod.ts",
        r#"
export type MyType = typeof MyClass;
export type MyTypeProp = typeof MyClass.staticProp;
export type MyTypeIndexAccess = typeof MyClass["staticProp"];
export type PrototypeAccess = typeof MyClass.prototype.instanceProp;

export class MyClass {
  instanceProp: string = "";
  static staticProp: string = "";
}
"#,
      );
    })
    .build_for_symbols()
    .await;

  let root_symbol = result.root_symbol();
  let module = root_symbol
    .module_from_specifier(&ModuleSpecifier::parse("file:///mod.ts").unwrap())
    .unwrap();
  let exports = module.exports(&root_symbol);

  let resolve_single_definition_text = |name: &str| -> String {
    let resolved_type = exports.resolved.get(name).unwrap();
    let resolved_type = resolved_type.as_resolved_export();
    let type_symbol = resolved_type.symbol();
    let deps = type_symbol
      .decls()
      .iter()
      .filter_map(|d| d.maybe_node())
      .flat_map(|s| s.deps())
      .collect::<Vec<_>>();
    assert_eq!(deps.len(), 1);
    let mut resolved_deps = root_symbol.resolve_symbol_dep(
      resolved_type.module,
      type_symbol,
      &deps[0],
    );
    assert_eq!(resolved_deps.len(), 1);
    let resolved_dep = resolved_deps.remove(0);
    let path = match resolved_dep {
      ResolvedSymbolDepEntry::Path(p) => p,
      ResolvedSymbolDepEntry::ImportType(_) => unreachable!(),
    };
    let definitions = path.into_definitions().collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let definition = &definitions[0];
    definition.text().to_string()
  };

  let class_text =
    "export class MyClass {\n  instanceProp: string = \"\";\n  static staticProp: string = \"\";\n}";
  assert_eq!(resolve_single_definition_text("MyType"), class_text);
  assert_eq!(
    resolve_single_definition_text("MyTypeProp"),
    "static staticProp: string = \"\";"
  );
  assert_eq!(
    resolve_single_definition_text("MyTypeIndexAccess"),
    // good enough for now
    class_text
  );
  assert_eq!(
    resolve_single_definition_text("PrototypeAccess"),
    // good enough for now
    class_text
  );
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn test_symbols_re_export_external() {
  let result = TestBuilder::new()
    .with_loader(|loader| {
      loader.remote.add_source_with_text(
        "file:///mod.ts",
        r#"export * from 'npm:example';"#,
      );
      loader.remote.add_external_source("npm:example");
    })
    .build_for_symbols()
    .await;

  let root_symbol = result.root_symbol();
  let module = root_symbol
    .module_from_specifier(&ModuleSpecifier::parse("file:///mod.ts").unwrap())
    .unwrap();
  let exports = module.exports(&root_symbol);
  assert_eq!(
    exports
      .unresolved_specifiers
      .into_iter()
      .map(|s| s.specifier.as_str())
      .collect::<Vec<_>>(),
    vec!["npm:example"]
  );
}

#[tokio::test]
async fn test_npm_version_not_found_then_found() {
  #[derive(Debug)]
  struct TestNpmResolver {
    made_first_request: Rc<RefCell<bool>>,
    should_never_succeed: bool,
    number_times_load_called: Rc<RefCell<u32>>,
  }

  impl NpmResolver for TestNpmResolver {
    fn resolve_builtin_node_module(
      &self,
      _specifier: &ModuleSpecifier,
    ) -> Result<Option<String>, UnknownBuiltInNodeModuleError> {
      Ok(None)
    }

    fn on_resolve_bare_builtin_node_module(
      &self,
      _module_name: &str,
      _range: &Range,
    ) {
    }

    fn load_and_cache_npm_package_info(
      &self,
      _package_name: &str,
    ) -> LocalBoxFuture<'static, Result<(), anyhow::Error>> {
      *self.number_times_load_called.borrow_mut() += 1;
      Box::pin(futures::future::ready(Ok(())))
    }

    fn resolve_npm(&self, package_req: &PackageReq) -> NpmPackageReqResolution {
      let mut value = self.made_first_request.borrow_mut();
      if *value && !self.should_never_succeed {
        assert_eq!(*self.number_times_load_called.borrow(), 2);
        NpmPackageReqResolution::Ok(PackageNv {
          name: package_req.name.clone(),
          version: Version::parse_from_npm("1.0.0").unwrap(),
        })
      } else {
        *value = true;
        NpmPackageReqResolution::ReloadRegistryInfo(anyhow!(
          "failed to resolve"
        ))
      }
    }
  }

  let mut loader = MemoryLoader::default();
  loader.add_source_with_text("file:///main.ts", "import 'npm:foo@1.0';");
  let root = ModuleSpecifier::parse("file:///main.ts").unwrap();

  {
    let npm_resolver = TestNpmResolver {
      made_first_request: Rc::new(RefCell::new(false)),
      number_times_load_called: Rc::new(RefCell::new(0)),
      should_never_succeed: false,
    };

    let mut graph = ModuleGraph::new(GraphKind::All);
    graph
      .build(
        vec![root.clone()],
        &mut loader,
        BuildOptions {
          npm_resolver: Some(&npm_resolver),
          ..Default::default()
        },
      )
      .await;
    assert!(graph.valid().is_ok());
    assert_eq!(
      graph
        .modules()
        .map(|m| m.specifier().to_string())
        .collect::<Vec<_>>(),
      vec![root.as_str(), "npm:/foo@1.0.0"]
    );
  }

  // now try never succeeding
  {
    let npm_resolver = TestNpmResolver {
      made_first_request: Rc::new(RefCell::new(false)),
      number_times_load_called: Rc::new(RefCell::new(0)),
      should_never_succeed: true,
    };

    let mut graph = ModuleGraph::new(GraphKind::All);
    graph
      .build(
        vec![root.clone()],
        &mut loader,
        BuildOptions {
          npm_resolver: Some(&npm_resolver),
          ..Default::default()
        },
      )
      .await;
    assert_eq!(
      graph.valid().err().unwrap().to_string(),
      "failed to resolve"
    );
  }
}

#[tokio::test]
async fn test_jsr_version_not_found_then_found() {
  #[derive(Default)]
  struct TestLoader {
    requests: Vec<(String, CacheSetting)>,
  }

  impl deno_graph::source::Loader for TestLoader {
    fn load(
      &mut self,
      specifier: &ModuleSpecifier,
      is_dynamic: bool,
      cache_setting: CacheSetting,
    ) -> LoadFuture {
      assert!(!is_dynamic);
      self.requests.push((specifier.to_string(), cache_setting));
      let specifier = specifier.clone();
      match specifier.as_str() {
        "file:///main.ts" => Box::pin(async move {
          Ok(Some(LoadResponse::Module {
            specifier: specifier.clone(),
            maybe_headers: None,
            content: "import 'jsr:@scope/a@1.2".into(),
          }))
        }),
        "https://registry-staging.deno.com/@scope/a/meta.json" => {
          Box::pin(async move {
            Ok(Some(LoadResponse::Module {
              specifier: specifier.clone(),
              maybe_headers: None,
              content: match cache_setting {
                CacheSetting::Only | CacheSetting::Use => {
                  // first time it won't have the version
                  r#"{ "versions": { "1.0.0": {} } }"#.into()
                }
                CacheSetting::Reload => {
                  // then on reload it will
                  r#"{ "versions": { "1.0.0": {}, "1.2.0": {} } }"#.into()
                }
              },
            }))
          })
        }
        "https://registry-staging.deno.com/@scope/a/1.2.0_meta.json" => {
          Box::pin(async move {
            Ok(Some(LoadResponse::Module {
              specifier: specifier.clone(),
              maybe_headers: None,
              content: r#"{ "exports": { ".": "./mod.ts" } }"#.into(),
            }))
          })
        }
        "https://registry-staging.deno.com/@scope/a/1.2.0/mod.ts" => {
          Box::pin(async move {
            Ok(Some(LoadResponse::Module {
              specifier: specifier.clone(),
              maybe_headers: None,
              content: "console.log('Hello, world!')".into(),
            }))
          })
        }
        _ => unreachable!(),
      }
    }
  }

  let mut loader = TestLoader::default();
  let mut graph = ModuleGraph::new(GraphKind::All);
  graph
    .build(
      vec![Url::parse("file:///main.ts").unwrap()],
      &mut loader,
      Default::default(),
    )
    .await;
  graph.valid().unwrap();
  assert_eq!(
    loader.requests,
    vec![
      ("file:///main.ts".to_string(), CacheSetting::Use),
      (
        "https://registry-staging.deno.com/@scope/a/meta.json".to_string(),
        CacheSetting::Use
      ),
      ("file:///main.ts".to_string(), CacheSetting::Use),
      (
        "https://registry-staging.deno.com/@scope/a/meta.json".to_string(),
        CacheSetting::Reload
      ),
      (
        "https://registry-staging.deno.com/@scope/a/1.2.0_meta.json"
          .to_string(),
        CacheSetting::Reload
      ),
      (
        "https://registry-staging.deno.com/@scope/a/1.2.0/mod.ts".to_string(),
        CacheSetting::Use
      ),
    ]
  );
}

#[tokio::test]
async fn test_dynamic_imports_with_template_arg() {
  async fn run_test(
    code: &str,
    files: Vec<(&str, &str)>,
    expected_specifiers: Vec<&str>,
  ) {
    let mut loader = MemoryLoader::default();
    let mut file_system = MemoryFileSystem::default();
    for (specifier, text) in &files {
      file_system.add_file(ModuleSpecifier::parse(specifier).unwrap());
      loader.add_source_with_text(specifier, text);
    }
    loader.add_source_with_text("file:///main.ts", code);
    let mut graph = ModuleGraph::new(GraphKind::All);
    graph
      .build(
        vec![Url::parse("file:///main.ts").unwrap()],
        &mut loader,
        BuildOptions {
          file_system: Some(&file_system),
          ..Default::default()
        },
      )
      .await;
    graph.valid().unwrap();

    let specifiers = graph
      .specifiers()
      .map(|s| s.0.to_string())
      .filter(|s| s != "file:///main.ts")
      .collect::<Vec<_>>();
    assert_eq!(specifiers, expected_specifiers);
  }

  // relative with ./
  run_test(
    "
await import(`./${test}`);
",
    vec![
      ("file:///a/mod.ts", ""),
      ("file:///a/sub_dir/a.ts", ""),
      ("file:///b.ts", ""),
    ],
    vec!["file:///a/mod.ts", "file:///a/sub_dir/a.ts", "file:///b.ts"],
  )
  .await;

  // relative with sub dir
  run_test(
    "
await import(`./a/${test}`);
",
    vec![
      ("file:///a/mod.ts", ""),
      ("file:///a/sub_dir/a.ts", ""),
      ("file:///b.ts", ""),
    ],
    vec!["file:///a/mod.ts", "file:///a/sub_dir/a.ts"],
  )
  .await;

  run_test(
    "
  // should not match these two because it does not end in a slash
  await import(`./b${test}`);
  await import(`./c/a${test}`);
  ",
    vec![
      ("file:///a/mod.ts", ""),
      ("file:///b.ts", ""),
      ("file:///c/a.ts", ""),
      ("file:///c/a/a.ts", ""),
    ],
    vec![],
  )
  .await;

  run_test(
    "
  await import(`./d/other/${test}/main.json`, {
    with: {
      type: 'json',
    },
  });
  await import(`./d/sub/${test}`);
  ",
    vec![
      ("file:///d/a.ts", ""),
      ("file:///d/sub/main.json", ""),
      ("file:///d/sub/a.ts", ""),
      ("file:///d/sub/a.js", ""),
      ("file:///d/sub/a.mjs", ""),
      ("file:///d/sub/a.mts", ""),
      // should not match because it's a declaration file
      ("file:///d/sub/a.d.ts", ""),
      ("file:///d/other/json/main.json", ""),
      ("file:///d/other/json/main2.json", ""),
    ],
    vec![
      "file:///d/other/json/main.json",
      "file:///d/sub/a.js",
      "file:///d/sub/a.mjs",
      "file:///d/sub/a.mts",
      "file:///d/sub/a.ts",
    ],
  )
  .await;

  // only matching one extension
  run_test(
    "
  await import(`./d/sub2/${test}.mjs`);
  ",
    vec![
      ("file:///d/sub2/a.ts", ""),
      ("file:///d/sub2/a.js", ""),
      ("file:///d/sub2/a.mjs", ""),
      ("file:///d/sub2/a.mts", ""),
    ],
    vec!["file:///d/sub2/a.mjs"],
  )
  .await;

  // file specifiers
  run_test(
    "await import(`file:///other/${test}`);",
    vec![("file:///other/mod.ts", ""), ("file:///b.ts", "")],
    vec!["file:///other/mod.ts"],
  )
  .await;

  // multiple exprs with same string between
  run_test(
    "await import(`./other/${test}/other/${test}/mod.ts`);",
    vec![
      ("file:///other/mod.ts", ""),
      ("file:///other/other/mod.ts", ""),
      ("file:///other/test/other/mod.ts", ""),
      ("file:///other/test/other/test/mod.ts", ""),
      ("file:///other/test/other/test/other/mod.ts", ""),
      ("file:///b.ts", ""),
    ],
    vec![
      "file:///other/test/other/test/mod.ts",
      "file:///other/test/other/test/other/mod.ts",
    ],
  )
  .await;

  // finding itself
  run_test(
    "await import(`./${expr}`);",
    vec![
      ("file:///main.ts", ""), // self
      ("file:///other.ts", ""),
    ],
    // should not have "file:///" here
    vec!["file:///other.ts"],
  )
  .await;
}
