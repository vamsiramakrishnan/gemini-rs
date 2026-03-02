//! Reader for js-genai REST module classes.
//!
//! Extracts REST API method signatures from TypeScript module files
//! (e.g., `files.ts`, `caches.ts`, `batches.ts`, `tunings.ts`).

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

use crate::schema::genai::{HttpMethod, RestMethodDef, RestModuleDef};

/// Which TypeScript files to scan for REST modules.
#[allow(dead_code)]
const REST_MODULE_FILES: &[&str] = &[
    "files.ts",
    "caches.ts",
    "batches.ts",
    "models.ts",
];

// Note: tunings.ts doesn't exist in js-genai; it's called tunings in the module
// but the actual TS methods are in a file we detect via class name pattern.

/// Read REST module TypeScript files and extract method definitions.
pub fn read_rest_modules(source_dir: &Path) -> Result<Vec<RestModuleDef>, String> {
    let mut modules = Vec::new();

    // Scan all .ts files for classes extending BaseModule
    let entries: Vec<_> = std::fs::read_dir(source_dir)
        .map_err(|e| format!("Failed to read source dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "ts")
        })
        .collect();

    let class_re = Regex::new(r"export\s+class\s+(\w+)\s+extends\s+BaseModule")
        .map_err(|e| format!("Regex error: {e}"))?;

    for entry in &entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        if let Some(caps) = class_re.captures(&content) {
            let class_name = caps[1].to_string();
            let module_name = class_name.to_lowercase();

            // Skip modules we don't have mappings for
            if service_endpoint_map().get(module_name.as_str()).is_none() {
                continue;
            }

            let methods = extract_methods(&content, &module_name)?;
            let service_endpoint = service_endpoint_map()
                .get(module_name.as_str())
                .unwrap()
                .to_string();
            let error_type = error_type_map()
                .get(module_name.as_str())
                .unwrap_or(&"ApiError")
                .to_string();

            modules.push(RestModuleDef {
                name: module_name,
                class_name,
                service_endpoint,
                methods,
                error_type,
            });
        }
    }

    modules.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(modules)
}

/// Extract public method signatures from a REST module class.
fn extract_methods(content: &str, module_name: &str) -> Result<Vec<RestMethodDef>, String> {
    let method_names = method_name_map();
    let method_http = method_http_map();

    // Pattern A: arrow async methods
    //   list = async (\n    params: types.ListFilesParameters = {},\n  ): Promise<Pager<types.File>> =>
    //   create = async (\n    params: types.CreateBatchJobParameters,\n  ): Promise<types.BatchJob> =>
    let arrow_re = Regex::new(
        r"(?m)^\s+(\w+)\s*=\s*async\s*\("
    ).map_err(|e| format!("Regex error: {e}"))?;

    // Pattern B: regular async methods
    //   async get(params: types.GetFileParameters): Promise<types.File>
    //   async delete(\n    params: types.DeleteFileParameters,\n  ): Promise<types.DeleteFileResponse>
    let async_re = Regex::new(
        r"(?m)^\s+async\s+(\w+)\s*\("
    ).map_err(|e| format!("Regex error: {e}"))?;

    let mut seen = std::collections::HashSet::new();
    let mut methods = Vec::new();

    // Extract all method names (arrow form)
    for caps in arrow_re.captures_iter(content) {
        let ts_name = caps[1].to_string();
        if !seen.insert(ts_name.clone()) {
            continue;
        }
        if ts_name.starts_with('_') || is_private_method(&ts_name, content) {
            continue;
        }
        if let Some(method) = build_method_def(&ts_name, module_name, &method_names, &method_http, content) {
            methods.push(method);
        }
    }

    // Extract all method names (regular async form)
    for caps in async_re.captures_iter(content) {
        let ts_name = caps[1].to_string();
        if !seen.insert(ts_name.clone()) {
            continue;
        }
        if ts_name.starts_with('_') || is_private_method(&ts_name, content) {
            continue;
        }
        if let Some(method) = build_method_def(&ts_name, module_name, &method_names, &method_http, content) {
            methods.push(method);
        }
    }

    Ok(methods)
}

/// Check if a method is private/internal (preceded by `private` keyword).
fn is_private_method(name: &str, content: &str) -> bool {
    // Check for "private async methodName" or "private methodName ="
    let pattern = format!(r"private\s+(?:async\s+)?{name}\s*[\(=]");
    Regex::new(&pattern)
        .map(|re| re.is_match(content))
        .unwrap_or(false)
}

/// Build a RestMethodDef from a method name using static mappings.
fn build_method_def(
    ts_name: &str,
    module_name: &str,
    method_names: &HashMap<(&str, &str), &str>,
    method_http: &HashMap<(&str, &str), (HttpMethod, bool, bool)>,
    content: &str,
) -> Option<RestMethodDef> {
    let key = (module_name, ts_name);
    let rust_name = method_names
        .get(&key)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}_{}", ts_name, singular(module_name)));

    let (http_method, returns_void, is_special) = method_http
        .get(&key)
        .copied()
        .unwrap_or((HttpMethod::Get, false, false));

    // Extract JSDoc for this method
    let description = extract_jsdoc_for_method(ts_name, content);

    // Determine return type from mapping
    let return_type = infer_return_type(ts_name, module_name);

    Some(RestMethodDef {
        ts_name: ts_name.to_string(),
        rust_name,
        http_method,
        return_type,
        description,
        is_special,
        returns_void,
    })
}

/// Extract JSDoc comment preceding a method.
fn extract_jsdoc_for_method(method_name: &str, content: &str) -> Option<String> {
    // Look for /** ... */ block immediately before the method
    let pattern = format!(
        r"/\*\*\s*\n\s*\*\s*([^\n]+)(?:\n\s*\*[^\n]*)*\n\s*\*/\s*\n\s*(?:async\s+)?{method_name}\s*[\(=]"
    );
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(content))
        .map(|caps| caps[1].trim().to_string())
}

/// Infer the return type based on method name and module.
fn infer_return_type(ts_name: &str, module_name: &str) -> String {
    let resource = capitalize(singular(module_name));
    match ts_name {
        "list" => format!("List{}Response", capitalize(module_name)),
        "get" => resource,
        "create" | "tune" => resource,
        "update" => resource,
        "delete" | "cancel" => String::new(), // void
        "upload" => resource,
        "download" => "Vec<u8>".to_string(),
        "createEmbeddings" => resource,
        "registerFiles" => format!("Vec<{resource}>"),
        _ => "serde_json::Value".to_string(),
    }
}

/// Get singular form of a module name.
fn singular(module_name: &str) -> &str {
    match module_name {
        "files" => "file",
        "caches" => "cached_content",
        "tunings" => "tuning_job",
        "batches" => "batch_job",
        "models" => "model",
        "tokens" => "tokens",
        _ => module_name,
    }
}

/// Convert snake_case to PascalCase.
fn capitalize(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

// ── Static Mapping Tables ───────────────────────────────────────────────────

/// Maps (module_name, ts_method) → rust_method_name.
fn method_name_map() -> HashMap<(&'static str, &'static str), &'static str> {
    let mut m = HashMap::new();
    // Files
    m.insert(("files", "get"), "get_file");
    m.insert(("files", "list"), "list_files");
    m.insert(("files", "delete"), "delete_file");
    m.insert(("files", "upload"), "upload_file");
    m.insert(("files", "download"), "download_file");
    m.insert(("files", "registerFiles"), "register_files");
    // Caches
    m.insert(("caches", "get"), "get_cached_content");
    m.insert(("caches", "list"), "list_cached_contents");
    m.insert(("caches", "create"), "create_cached_content");
    m.insert(("caches", "update"), "update_cached_content");
    m.insert(("caches", "delete"), "delete_cached_content");
    // Batches
    m.insert(("batches", "get"), "get_batch_job");
    m.insert(("batches", "list"), "list_batch_jobs");
    m.insert(("batches", "create"), "create_batch_job");
    m.insert(("batches", "createEmbeddings"), "create_batch_embeddings");
    m.insert(("batches", "cancel"), "cancel_batch_job");
    m.insert(("batches", "delete"), "delete_batch_job");
    // Tunings
    m.insert(("tunings", "list"), "list_tuning_jobs");
    m.insert(("tunings", "get"), "get_tuning_job");
    m.insert(("tunings", "tune"), "create_tuning_job");
    m.insert(("tunings", "cancel"), "cancel_tuning_job");
    // Models
    m.insert(("models", "get"), "get_model");
    m.insert(("models", "list"), "list_models");
    m
}

/// Maps (module_name, ts_method) → (HttpMethod, returns_void, is_special).
fn method_http_map() -> HashMap<(&'static str, &'static str), (HttpMethod, bool, bool)> {
    let mut m = HashMap::new();
    // Files
    m.insert(("files", "list"),            (HttpMethod::Get, false, false));
    m.insert(("files", "get"),             (HttpMethod::Get, false, false));
    m.insert(("files", "delete"),          (HttpMethod::Delete, true, false));
    m.insert(("files", "upload"),          (HttpMethod::Post, false, true));
    m.insert(("files", "download"),        (HttpMethod::Get, false, true));
    m.insert(("files", "registerFiles"),   (HttpMethod::Post, false, true));
    // Caches
    m.insert(("caches", "list"),           (HttpMethod::Get, false, false));
    m.insert(("caches", "get"),            (HttpMethod::Get, false, false));
    m.insert(("caches", "create"),         (HttpMethod::Post, false, false));
    m.insert(("caches", "update"),         (HttpMethod::Patch, false, false));
    m.insert(("caches", "delete"),         (HttpMethod::Delete, true, false));
    // Batches
    m.insert(("batches", "list"),          (HttpMethod::Get, false, false));
    m.insert(("batches", "get"),           (HttpMethod::Get, false, false));
    m.insert(("batches", "create"),        (HttpMethod::Post, false, false));
    m.insert(("batches", "createEmbeddings"), (HttpMethod::Post, false, false));
    m.insert(("batches", "cancel"),        (HttpMethod::Post, true, false));
    m.insert(("batches", "delete"),        (HttpMethod::Delete, true, false));
    // Tunings
    m.insert(("tunings", "list"),          (HttpMethod::Get, false, false));
    m.insert(("tunings", "get"),           (HttpMethod::Get, false, false));
    m.insert(("tunings", "tune"),          (HttpMethod::Post, false, false));
    m.insert(("tunings", "cancel"),        (HttpMethod::Post, true, false));
    // Models
    m.insert(("models", "list"),           (HttpMethod::Get, false, false));
    m.insert(("models", "get"),            (HttpMethod::Get, false, false));
    m
}

/// Maps module name → ServiceEndpoint variant name.
fn service_endpoint_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("files", "Files");
    m.insert("caches", "CachedContents");
    m.insert("batches", "BatchJobs");
    m.insert("tunings", "TuningJobs");
    m.insert("models", "ListModels");
    m
}

/// Maps module name → error type name.
fn error_type_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("files", "FilesError");
    m.insert("caches", "CachesError");
    m.insert("batches", "BatchesError");
    m.insert("tunings", "TuningsError");
    m.insert("models", "ModelsError");
    m
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_FILES_TS: &str = r#"
import {ApiClient} from './_api_client.js';
import {BaseModule} from './_common.js';
import * as types from './types.js';

export class Files extends BaseModule {
  constructor(private readonly apiClient: ApiClient) {
    super();
  }

  /**
   * Lists files.
   */
  list = async (
    params: types.ListFilesParameters = {},
  ): Promise<Pager<types.File>> => {
    return new Pager<types.File>(
      PagedItem.PAGED_ITEM_FILES,
      (x: types.ListFilesParameters) => this.listInternal(x),
      await this.listInternal(params),
      params,
    );
  };

  async upload(params: types.UploadFileParameters): Promise<types.File> {
    return this.apiClient.uploadFile(params.file, params.config);
  }

  /**
   * Retrieves the file information from the service.
   */
  async get(params: types.GetFileParameters): Promise<types.File> {
    let response: Promise<types.File>;
    // ...
  }

  async delete(
    params: types.DeleteFileParameters,
  ): Promise<types.DeleteFileResponse> {
    let response: Promise<types.DeleteFileResponse>;
    // ...
  }

  private async listInternal(
    params: types.ListFilesParameters,
  ): Promise<types.ListFilesResponse> {
    // ...
  }

  private async createInternal(
    params: types.CreateFileParameters,
  ): Promise<types.CreateFileResponse> {
    // ...
  }
}
"#;

    #[test]
    fn extracts_class_name() {
        let class_re = Regex::new(r"export\s+class\s+(\w+)\s+extends\s+BaseModule").unwrap();
        let caps = class_re.captures(SAMPLE_FILES_TS).unwrap();
        assert_eq!(&caps[1], "Files");
    }

    #[test]
    fn extracts_public_methods() {
        let methods = extract_methods(SAMPLE_FILES_TS, "files").unwrap();
        let names: Vec<&str> = methods.iter().map(|m| m.ts_name.as_str()).collect();
        assert!(names.contains(&"list"), "Should contain list: {:?}", names);
        assert!(names.contains(&"get"), "Should contain get: {:?}", names);
        assert!(names.contains(&"delete"), "Should contain delete: {:?}", names);
        assert!(names.contains(&"upload"), "Should contain upload: {:?}", names);
        // Should NOT contain private methods
        assert!(!names.contains(&"listInternal"), "Should not contain listInternal");
        assert!(!names.contains(&"createInternal"), "Should not contain createInternal");
    }

    #[test]
    fn maps_rust_names() {
        let methods = extract_methods(SAMPLE_FILES_TS, "files").unwrap();
        let get = methods.iter().find(|m| m.ts_name == "get").unwrap();
        assert_eq!(get.rust_name, "get_file");
        let list = methods.iter().find(|m| m.ts_name == "list").unwrap();
        assert_eq!(list.rust_name, "list_files");
        let del = methods.iter().find(|m| m.ts_name == "delete").unwrap();
        assert_eq!(del.rust_name, "delete_file");
    }

    #[test]
    fn maps_http_methods() {
        let methods = extract_methods(SAMPLE_FILES_TS, "files").unwrap();
        let get = methods.iter().find(|m| m.ts_name == "get").unwrap();
        assert_eq!(get.http_method, HttpMethod::Get);
        let del = methods.iter().find(|m| m.ts_name == "delete").unwrap();
        assert_eq!(del.http_method, HttpMethod::Delete);
        assert!(del.returns_void);
    }

    #[test]
    fn flags_special_methods() {
        let methods = extract_methods(SAMPLE_FILES_TS, "files").unwrap();
        let upload = methods.iter().find(|m| m.ts_name == "upload").unwrap();
        assert!(upload.is_special);
    }

    #[test]
    fn extracts_jsdoc() {
        let methods = extract_methods(SAMPLE_FILES_TS, "files").unwrap();
        let list = methods.iter().find(|m| m.ts_name == "list").unwrap();
        assert!(list.description.is_some(), "list should have JSDoc");
        assert!(list.description.as_ref().unwrap().contains("Lists files"));
    }

    #[test]
    fn singular_module_names() {
        assert_eq!(singular("files"), "file");
        assert_eq!(singular("caches"), "cached_content");
        assert_eq!(singular("batches"), "batch_job");
    }

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("file"), "File");
        assert_eq!(capitalize("cached_content"), "CachedContent");
        assert_eq!(capitalize("batch_job"), "BatchJob");
    }

    #[test]
    fn infer_return_types() {
        assert_eq!(infer_return_type("list", "files"), "ListFilesResponse");
        assert_eq!(infer_return_type("get", "files"), "File");
        assert_eq!(infer_return_type("delete", "files"), "");
        assert_eq!(infer_return_type("list", "caches"), "ListCachesResponse");
        assert_eq!(infer_return_type("create", "caches"), "CachedContent");
    }
}
