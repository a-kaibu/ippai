// node: `node sake.js program.exe [args]` runs the program, and the processes it starts run in
// this node process too, each its own instance of sake (host/pre.js passes them the kernel
// they share). Node exits with the first program's exit code once all of them ended.
if (typeof require == 'function' && require.main === module && require('worker_threads').isMainThread) {
  let live = 0;
  let first;
  const start = (args, env, kernel) => {
    live++;
    const root = !kernel;
    createSake({
      arguments: args,
      vnvmEnv: env,
      vnvmKernel: kernel,
      vnvmSpawn: (req, k) => start([req.exe], { SAKE_PID: String(req.pid) }, k),
      quit: (code) => {
        if (root) first = code;
        if (--live === 0) process.exit(first ?? code);
      },
    }).catch((e) => {
      console.error(`sake: ${e}`);
      if (--live === 0) process.exit(root ? 1 : first ?? 1);
    });
  };
  start(process.argv.slice(2));
}
