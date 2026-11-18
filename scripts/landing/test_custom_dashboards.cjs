const assert = require('node:assert/strict');
const {test} = require('node:test');
const path = require('node:path');
const {mount} = require('./playground_fixture.cjs');
const bundle = path.join(__dirname, '../../landing/playground/app.bundle.js');
function page(custom, id = 'user-copy') {
 return mount(bundle, { hash: '#/dashboards/' + id, storage: {
  'xerj.dashboards': JSON.stringify({order:Object.keys(custom),custom}),
  'xerj.edit':'1',
 }});
}
for (const [template, label] of [['rag-quality','GROUNDING SCORE'],['logs-overview','EVENTS'],['system','CPU']]) {
 test(`a ${template} clone receives its template data`,async()=>{
  const p=page({'user-copy':{name:'My dashboard',clonedFrom:template}});await p.flush();
  assert.match(p.app.innerHTML,/MY DASHBOARD/);
  assert.ok(p.app.innerHTML.includes(label));
  assert.doesNotMatch(p.app.innerHTML,/Cannot read properties|Cannot convert undefined|\.map is not a function/);
 });
}
test('a clone of a user dashboard retains its underlying template',async()=>{
 const p=page({'user-original':{name:'First RAG',clonedFrom:'rag-quality'},'user-copy':{name:'Second RAG',clonedFrom:'user-original'}});await p.flush();
 assert.match(p.app.innerHTML,/GROUNDING SCORE/);
 assert.doesNotMatch(p.app.innerHTML,/NO TEMPLATE|Cannot read properties/);
});
test('adding a panel to a custom dashboard persists the addition',async()=>{
 const p=page({'user-copy':{name:'My dashboard',clonedFrom:'ai-overview'}});await p.flush();
 await p.click('data-add','metric');
 const layout=JSON.parse(p.localStorage['xerj.layout.user-copy']);
 assert.equal(layout.added.length,1);assert.equal(layout.added[0].type,'metric');
});
test('custom dashboard panels can be resized and removed',async()=>{
 const p=page({'user-copy':{name:'My dashboard',clonedFrom:'rag-quality'}});await p.flush();
 await p.click({'data-size':'6','data-panel':'grounding'});
 let layout=JSON.parse(p.localStorage['xerj.layout.user-copy']);assert.equal(layout.cols.grounding,6);
 await p.click({'data-remove':'','data-panel':'grounding'});
 layout=JSON.parse(p.localStorage['xerj.layout.user-copy']);assert.ok(layout.hidden.includes('grounding'));
});
test('cyclic user templates terminate in the blank fallback',async()=>{
 const p=page({'user-copy':{name:'Cycle',clonedFrom:'user-other'},'user-other':{name:'Other',clonedFrom:'user-copy'}});await p.flush();assert.match(p.app.innerHTML,/NO TEMPLATE/);
});
test('a missing template keeps the documented blank fallback',async()=>{
 const p=page({'user-copy':{name:'Orphan',clonedFrom:'removed-template'}});await p.flush();assert.match(p.app.innerHTML,/NO TEMPLATE/);
});

for (const [id, custom] of [
 ['user-copy', {'user-copy':{name:'My RAG',clonedFrom:'rag-quality'}}],
 ['rag-quality', {}],
]) {
 test(`${id} retains resized, hidden and added panels after a fresh boot`,async()=>{
  const p=page(custom,id);await p.flush();
  await p.click({'data-size':'6','data-panel':'grounding'});
  await p.click({'data-remove':'','data-panel':'halluc'});
  await p.click('data-add','metric');
  const stored=JSON.parse(p.localStorage['xerj.layout.'+id]);
  assert.equal(stored.cols.grounding,6);assert.ok(stored.hidden.includes('halluc'));
  const restored=mount(bundle,{hash:'#/dashboards/'+id,storage:{...p.localStorage}});await restored.flush();
  assert.doesNotMatch(restored.app.innerHTML,/HALLUCINATION RATE/);
  assert.ok(restored.app.innerHTML.includes(stored.added[0].id));
  assert.match(restored.app.innerHTML,/data-panel="grounding"[^>]*style="grid-column: span 6/);
 });
}

test('the active custom dashboard is exposed in its expanded navigation group',async()=>{
 const p=page({'user-copy':{name:'My dashboard',clonedFrom:'ai-overview'}});await p.flush();
 assert.match(p.app.innerHTML,/<button[^>]+data-dash-group="other"[^>]+aria-expanded="true"/);
 assert.match(p.app.innerHTML,/data-dash="user-copy" class="active"/);
});
test('default dashboards retain their expanded registered navigation group',async()=>{
 const p=page({},'ai-overview');await p.flush();assert.match(p.app.innerHTML,/<button[^>]+data-dash-group="ai"[^>]+aria-expanded="true"/);
});
