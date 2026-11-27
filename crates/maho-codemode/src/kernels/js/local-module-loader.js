globalThis.__senpi_reserved_agent_tool__ = globalThis.__senpi_module_context__.reservedAgentTool;
globalThis.__senpi_reserved_output_tool__ = globalThis.__senpi_module_context__.reservedOutputTool;
globalThis.__senpi_reserved_schema_tool__ = globalThis.__senpi_module_context__.reservedSchemaTool;
globalThis.__senpi_call_handle__ = async (operation, fields) => await globalThis.tool(globalThis.__senpi_module_context__.reservedHandleTool)({ operation, ...fields });
globalThis.__senpi_timeout_pause_op__ = globalThis.__senpi_module_context__.timeoutPauseOp;
globalThis.__senpi_timeout_resume_op__ = globalThis.__senpi_module_context__.timeoutResumeOp;
globalThis.packages = Object.freeze({
  install: async (manager, requirements, options = {}) => {
    if (options === null || typeof options !== 'object' || Array.isArray(options)) throw new TypeError('packages.install(): options must be an object');
    return await globalThis.tool('__packages_install__')({ manager, requirements, ...(options.timeout !== undefined ? { timeout: options.timeout } : {}) });
  },
});
globalThis.__senpi_import__ = async (source, options) => {
  const context = globalThis.__senpi_module_context__;
  const specifier = String(source);
  const permissionImport = globalThis[Symbol.for('senpi.kernel.permissionImport')];
  permissionImport?.(specifier, new URL('__cell__.js', context.cwdUrl).href);
  const kernelBun = globalThis[Symbol.for('senpi.kernel.bun')];
  if (specifier === 'bun' && kernelBun) return { ...kernelBun, default: kernelBun };
  const patched = /^(?:node:)?(child_process|fs|fs\/promises|path)$/.exec(specifier);
  if (patched && typeof process.getBuiltinModule === 'function') {
    const builtin = process.getBuiltinModule('node:' + patched[1]);
    if (builtin) return { ...builtin, default: builtin };
  }
  const match = /^([a-z][a-z0-9+.-]*):\/\/(.*)$/i.exec(specifier);
  let target = specifier;
  if (match && match[1].toLowerCase() === 'file') {
    target = new URL(specifier).href;
  } else if (match) {
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
  } else {
    await globalThis.__senpi_package_resolver_ready__;
    target = globalThis[Symbol.for('senpi.kernel.resolvePackage')](specifier, [context.cwdUrl, context.packageRootUrl]) ?? specifier;
  }
  const permissionLoad = globalThis[Symbol.for('senpi.kernel.permissionLoad')];
  const run = (resolved) => (options === undefined ? import(resolved) : import(resolved, options));
  return permissionLoad ? permissionLoad(() => run(target)) : run(target);
};
