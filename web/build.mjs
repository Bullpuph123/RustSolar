// Builds web/dist from web/src (TypeScript, bundled by esbuild) and static files.
//   node build.mjs           production build
//   node build.mjs --watch   rebuild on change
import { build, context } from 'esbuild';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, 'dist');
const version = (readFileSync(join(here, '..', 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m) || [])[1] || '0.0.0';

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
copyFileSync(join(here, 'index.html'), join(dist, 'index.html'));
copyFileSync(join(here, 'public', 'logo.png'), join(dist, 'logo.png'));
copyFileSync(join(here, '..', 'brand', 'tokens.css'), join(dist, 'tokens.css'));
copyFileSync(join(here, 'src', 'app.css'), join(dist, 'app.css'));
copyFileSync(join(here, 'src', 'solar.css'), join(dist, 'solar.css'));
copyFileSync(join(here, 'node_modules', 'uplot', 'dist', 'uPlot.min.css'), join(dist, 'uplot.css'));
writeFileSync(join(dist, 'manifest.webmanifest'), JSON.stringify({
  name: 'RustSolar', short_name: 'RustSolar', start_url: '/', display: 'standalone',
  background_color: '#0F1B2D', theme_color: '#0F1B2D', icons: [{ src: '/logo.png', sizes: '96x96', type: 'image/png' }],
}));

const opts = {
  entryPoints: [join(here, 'src', 'main.ts')],
  bundle: true,
  minify: true,
  format: 'iife',
  target: ['es2020'],
  outfile: join(dist, 'app.js'),
  define: { __APP_VERSION__: JSON.stringify(version) },
  legalComments: 'none',
  logLevel: 'info',
};
if (process.argv.includes('--watch')) {
  const ctx = await context(opts);
  await ctx.watch();
} else {
  await build(opts);
}
