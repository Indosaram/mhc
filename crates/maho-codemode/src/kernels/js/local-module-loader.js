globalThis.__senpi_reserved_agent_tool__ = globalThis.__senpi_module_context__.reservedAgentTool;
globalThis.__senpi_reserved_output_tool__ = globalThis.__senpi_module_context__.reservedOutputTool;
globalThis.__senpi_reserved_schema_tool__ = globalThis.__senpi_module_context__.reservedSchemaTool;
globalThis.__senpi_timeout_pause_op__ = globalThis.__senpi_module_context__.timeoutPauseOp;
globalThis.__senpi_timeout_resume_op__ = globalThis.__senpi_module_context__.timeoutResumeOp;
globalThis.__senpi_import__ = async (source, options) => {
  const context = globalThis.__senpi_module_context__;
  const specifier = String(source);
  const match = /^([a-z][a-z0-9+.-]*):\/\/(.*)$/i.exec(specifier);
  let target = specifier;
  if (match) {
    const scheme = match[1].toLowerCase();
    const root = context.localRootUrls[scheme];
    if (!root) throw new Error('Unsupported module protocol: ' + specifier);
    let relative;
    try { relative = decodeURIComponent(match[2].replaceAll('\\', '/')); }
    catch { throw new Error('Invalid module URL encoding: ' + specifier); }
    if (relative.startsWith('/') || relative.split('/').includes('..')) {
      throw new Error('Module path escapes ' + scheme + ':// root: ' + specifier);
    }
    target = new URL(relative, root).href;
  } else if (specifier.startsWith('./') || specifier.startsWith('../') || specifier === '.' || specifier === '..') {
    target = new URL(specifier, context.cwdUrl).href;
  } else if (specifier.startsWith('/') || /^[A-Za-z]:[\\/]/.test(specifier)) {
    const urlModule = await import('node:url');
    target = urlModule.pathToFileURL(specifier).href;
  }
  return options === undefined ? import(target) : import(target, options);
};
