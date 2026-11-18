const fs = require('node:fs');
const vm = require('node:vm');
function mount(bundle, { hash = '#/dashboards/ai-overview', search = '', storage = {} } = {}) {
 const events = {}, pending = [], errors = [];
 const localStorage = { ...storage };
 Object.defineProperties(localStorage, {
  getItem: {value:key => localStorage[key] ?? null}, setItem:{value:(key,val) => localStorage[key]=String(val)}, removeItem:{value:key=>delete localStorage[key]},
 });
 let html = '', renders = 0;
 const app = { set innerHTML(v) { html=v; renders++; }, get innerHTML() { return html; }, setAttribute() {} };
 const document = { body: { scrollHeight: 1000 }, getElementById:()=>app, querySelector:()=>null, querySelectorAll:()=>[], addEventListener:(type,fn)=>(events[type]??=[]).push(fn), documentElement:{ getAttribute:()=> 'day', setAttribute(){}, classList:{add(){},toggle(){}} } };
 let currentHash=hash;
 const location = { pathname:'/playground/', search, get hash(){return currentHash;}, set hash(value){if(value!==currentHash){currentHash=value;pending.push(()=>events.hashchange?.forEach(fn=>fn()));}} };
 const history = { replaceState(_,__,url) { const u=new URL(url,'http://localhost'); location.search=u.search; currentHash=u.hash; } };
 const window = { addEventListener:(type,fn)=>(events[type]??=[]).push(fn), scrollTo(){} };
 const context = {document,localStorage,location,history,window,URLSearchParams,URL,console,performance,Date,Math,setInterval(){return 1;},clearInterval(){},requestAnimationFrame(fn){fn();},confirm(){return true;},prompt(){return 'saved';}};
 vm.runInNewContext(fs.readFileSync(bundle,'utf8'),context);
 return {app,localStorage,location,errors,get renders(){return renders;}, async flush(){for(let i=0;i<8;i++){await Promise.resolve();while(pending.length)pending.shift()();}}, async click(attr,value=''){const attrs=typeof attr==='object'?attr:{[attr]:value}; const el={getAttribute:k=>attrs[k]??null};const target={closest:selector=>Object.keys(attrs).some(k=>selector===`[${k}]`)?el:null};for(const fn of events.click||[])fn({target,preventDefault(){}});await this.flush();} };
}
module.exports={mount};
