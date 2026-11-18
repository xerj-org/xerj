const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');

// Run the actual export pipeline, replacing only browser primitives.
async function exportFixture() {
 const child = {name:'query', original:true};
 const source = {name:'root', original:true, cloneNode:()=>clone,
  getBoundingClientRect:()=>({width:600,height:180}),querySelectorAll:()=>[child]};
 const copied=[];
 const makeClone = name => ({name,attrs:{},style:{setProperty:(key,value)=>copied.push([name,key,value])},setAttribute(key,value){this.attrs[key]=value;},getAttribute(key){return this.attrs[key];},innerHTML:'<line stroke="var(--z-accent)"/>'});
 const clone=makeClone('root');const clonedChild=makeClone('query');clone.querySelectorAll=()=>[clonedChild];
 let downloaded=false;
 const body=[];
 const document={documentElement:{name:'page'},body:{appendChild:el=>body.push(el),removeChild:el=>body.splice(body.indexOf(el),1)},createElement:type=>type==='canvas'?{getContext:()=>({scale(){},drawImage(){}}),toBlob:cb=>cb({png:true})}:{click(){downloaded=true;}}};
 const styles={root:{color:'rgb(34, 34, 34)',fill:'rgb(34, 34, 34)',stroke:'none'},query:{color:'rgb(34, 34, 34)',stroke:'rgb(255, 204, 0)',fill:'none','stroke-opacity':'0.5'},page:{'--z-paper':'#ffffff'}};
 const getComputedStyle=el=>({color:styles[el.name].color,getPropertyValue:key=>styles[el.name][key]||''});
 const context={document,getComputedStyle,URL:{createObjectURL:()=> 'blob:fixture',revokeObjectURL(){}},setTimeout(){},XMLSerializer:class{serializeToString(){return '<svg/>';}},Image:class{set src(value){this.onload();}}};
 vm.createContext(context);
 const sourceCode=fs.readFileSync(path.join(__dirname,'../../landing/playground/src/data/export.js'),'utf8');
 vm.runInContext(sourceCode.replace(/export /g,''),context);
 await context.svgToPng(source,{scale:1});
 return {copied,clone,source,body,downloaded};
}
test('PNG snapshot resolves inherited query-marker paint styles before detaching the SVG',async()=>{
 const p=await exportFixture();
 assert.ok(p.copied.some(([name,key,value])=>name==='query'&&key==='stroke'&&value==='rgb(255, 204, 0)'));
 assert.ok(p.copied.some(([name,key,value])=>name==='query'&&key==='stroke-opacity'&&value==='0.5'));
});
test('export leaves the live SVG untouched and preserves download and cleanup',async()=>{
 const p=await exportFixture();assert.equal(p.source.original,true);assert.equal(p.source.style,undefined);assert.equal(p.downloaded,true);assert.equal(p.body.length,0);assert.equal(p.clone.attrs.width,'600');assert.equal(p.clone.attrs.height,'180');
});
