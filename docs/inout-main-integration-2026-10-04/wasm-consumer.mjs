import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
const proof = '/home/seith/REDA/target/qualification/main-inout-integration';
const candidate = path.join(proof, 'index-candidate');
const pkg = path.join(candidate, 'viewer/pkg');
const {initSync, Session, Axis} = await import(pathToFileURL(path.join(pkg, 'reda_viewer.js')));
initSync({module: fs.readFileSync(path.join(pkg, 'reda_viewer_bg.wasm'))});
const source = fs.readFileSync(path.join(candidate, 'tests/fixtures/resolved-inout.sv'), 'utf8');
const bundle = '/home/seith/REDA/docs/inout-native-cli-2026-10-04/native';
const bytes = new Uint8Array(fs.readFileSync(path.join(bundle, 'top.litematic')));
const pinout = JSON.parse(fs.readFileSync(path.join(bundle, 'top.pinout.json'), 'utf8'));
const session = Session.from_native_baked(source, 'top', bytes, pinout);
assert.deepEqual(session.inout_names(), ['bus']);
const pins = session.pinout();
assert.deepEqual(pins.inputs.map(p => p.name).sort(), ['d0', 'd1', 'e0', 'e1']);
const y = pins.outputs.find(p => p.name === 'y');
assert.ok(y);
const size = session.size();
const states = [];
for (let state = 0; state < 64; state++) {
    for (const [name, bit] of [['e0',0], ['d0',1], ['e1',2], ['d1',3]]) {
        session.set_lever(name, Boolean(state & (1 << bit)));
    }
    session.set_inout('bus', Boolean(state & 16), Boolean(state & 32));
    session.run_until_stable();
    const active = [[0,1], [2,3], [4,5]].filter(([e]) => state & (1 << e))
        .map(([,d]) => Boolean(state & (1 << d)));
    const expected = active.length === 0 ? 'Floating' : active.some(v => v !== active[0])
        ? 'Contention' : active[0];
    let observed;
    try { observed = session.read_inout('bus'); } catch (error) { observed = String(error); }
    assert.equal(observed, expected, `state=${state}`);
    let publicY = null;
    if (typeof expected === 'boolean') {
        const slice = session.slice(Axis.Y, y.y);
        publicY = slice[2 * (y.x * size[2] + y.z) + 1] > 0;
        assert.equal(publicY, expected, `public y state=${state}`);
    }
    states.push({state, expected, observed, publicY});
}
assert.throws(() => session.set_inout('missing', true, true));
assert.throws(() => session.set_lever('bus', true));
const wrong = structuredClone(pinout);
wrong.inouts[0].readback = [-1,0,0];
assert.throws(() => Session.from_native_baked(source, 'top', bytes, wrong));
session.reset();
assert.deepEqual(session.inout_names(), ['bus']);
fs.writeFileSync(path.join(proof, 'wasm-consumer-result.json'), JSON.stringify({
    runtime: process.version, states, controls: ['unknown inout', 'inout not ordinary input',
        'source-mismatched metadata', 'reset retains shared port'],
    scope: 'Actual generated WebAssembly bindings executed in Node; not browser or Minecraft evidence.'
}, null, 2) + '\n');
session.free();
console.log('WASM shared inout: 64 states and public y pass; consumer negative controls pass.');
