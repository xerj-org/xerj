const assert = require('node:assert/strict');
const {test} = require('node:test');
const path = require('node:path');
const {mount} = require('./playground_fixture.cjs');
const bundle = path.join(__dirname, '../../landing/playground/app.bundle.js');
const view = {id:'saved',dashId:'ai-overview',time:'7D',cluster:'prod-us',filters:{service:'auth'}};
function page(saved = view, options = {}) {
 return mount(bundle,{...options,storage:{...options.storage,'xerj.views':JSON.stringify([saved])}});
}
test('applying a view on the current dashboard repaints and updates the share URL',async()=>{
 const p=page();await p.flush();const before=p.renders;
 await p.click('data-view-apply','saved');
 assert.ok(p.renders>before);
 const params=new URLSearchParams(p.location.search);
 assert.equal(params.get('t'),'7D');assert.equal(params.get('c'),'prod-us');assert.equal(params.get('f'),'service:auth');
 assert.match(p.app.innerHTML,/PROD-US/);
});
test('cross-dashboard restoration wins over the previous URL context',async()=>{
 const p=page({...view,dashId:'rag-quality'},{search:'?t=1H&c=staging&f=service:old'});await p.flush();
 await p.click('data-view-apply','saved');
 assert.equal(p.location.hash,'#/dashboards/rag-quality');
 const params=new URLSearchParams(p.location.search);
 assert.equal(params.get('t'),'7D');assert.equal(params.get('c'),'prod-us');assert.equal(params.get('f'),'service:auth');
 assert.deepEqual(JSON.parse(p.localStorage['xerj.filters.rag-quality']),{service:'auth'});
});
test('custom saved views restore their endpoints and persist them for reload',async()=>{
 const timeCustom={from:'2026-01-01T00:00',to:'2026-01-03T00:00'};
 const p=page({...view,time:'CUSTOM',timeCustom},{storage:{'xerj.timeFrom':'2025-01-01T00:00','xerj.timeTo':'2025-01-02T00:00'}});await p.flush();await p.click('data-view-apply','saved');
 assert.equal(p.localStorage['xerj.timeFrom'],timeCustom.from);assert.equal(p.localStorage['xerj.timeTo'],timeCustom.to);
 const params=new URLSearchParams(p.location.search);assert.equal(params.get('tfrom'),timeCustom.from);assert.equal(params.get('tto'),timeCustom.to);
});
test('saving a custom view captures both endpoints',async()=>{
 const p=page(view,{search:'?t=CUSTOM&tfrom=2026-01-01T00:00&tto=2026-01-03T00:00'});await p.flush();await p.click('data-view-save');
 const saved=JSON.parse(p.localStorage['xerj.views']).at(-1);
 assert.deepEqual(saved.timeCustom,{from:'2026-01-01T00:00',to:'2026-01-03T00:00'});
});
test('unknown saved view is a no-op',async()=>{
 const p=page();await p.flush();const before=p.renders;await p.click('data-view-apply','missing');assert.equal(p.renders,before);
});
