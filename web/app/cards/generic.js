import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {CAT,DEVINFO,LAST,classOf,nameOf,isOwner,NR,stateOf,reasonOf,usable,pluginLabel} from '../core/store.js';
import {PENDING} from '../core/actions.js';
import {toast} from '../ui/toast.js';
import {ACT} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {row,failHtml} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {copyText} from '../console/rfb.js';
import {cardOf,iconOf,sentence,readings,deviceWord,notesBlock,wireNotes} from './index.js';

/* ============================================================ the generic card: for a class this page has no card for, and the page behind the plainer ones */
/* what it shows comes only from the API: the name, the state's fields, and every action the catalogue lets this person run on it, each with a form drawn from its parameters */
const actionsFor=t=>{const seen=new Set();return Object.values(CAT).filter(d=>{if(!d||seen.has(d.id)||d.id==='state.read'||!(d.targets||[]).includes(t))return false;seen.add(d.id);return true}).sort((a,b)=>String(a.label).localeCompare(String(b.label)))};
function paramInput(id,p){const s=p.schema||{};const key=esc(id)+'-'+esc(p.name);const label=sentence(String(p.name).replace(/_/g,' '));const help=p.help?`<small>${esc(p.help)}</small>`:'';
  if(s.type==='boolean')return `<div class="rowi pfield"><div class="t"><b>${esc(label)}</b>${help}</div><button class="toggle" role="switch" aria-checked="false" aria-label="${esc(label)}" data-param="${esc(p.name)}" data-kind="bool"></button></div>`;
  if(s.type==='enum')return `<label class="pfield"><span>${esc(label)}</span><select class="field" data-param="${esc(p.name)}" data-kind="enum" id="pf-${key}">${(s.values||[]).map(v=>`<option value="${esc(v)}">${esc(sentence(String(v).replace(/_/g,' ')))}</option>`).join('')}</select>${help}</label>`;
  if(s.type==='integer')return `<label class="pfield"><span>${esc(label)}${s.min!=null&&s.max!=null?` <span class="fine">${esc(s.min)} to ${esc(s.max)}</span>`:''}</span><input class="field" type="number" inputmode="numeric" step="1" ${s.min!=null?`min="${esc(s.min)}" value="${esc(s.min)}"`:''} ${s.max!=null?`max="${esc(s.max)}"`:''} data-param="${esc(p.name)}" data-kind="int" id="pf-${key}" autocomplete="off">${help}</label>`;
  return `<label class="pfield"><span>${esc(label)}</span><input class="field" type="text" ${s.max_len?`maxlength="${esc(s.max_len)}"`:''} data-param="${esc(p.name)}" data-kind="text" id="pf-${key}" autocomplete="off" spellcheck="false">${help}</label>`}
function actionForm(d){return `<div class="actform" data-actform="${esc(d.id)}"><div class="t"><b>${esc(d.label||d.id)}</b>${d.description?`<small>${esc(d.description)}</small>`:''}</div>${(d.params||[]).map(p=>paramInput(d.id,p)).join('')}<div class="acts" data-acts><button class="btn ${d.tier===3||d.tier==='S'?'danger':'primary'}" data-run="${esc(d.id)}">${esc(d.label||d.id)}</button></div><div data-result></div></div>`}
/* the values of one form, checked against the same schema the server checks */
function formValues(form,d){const out={};
  for(const p of d.params||[]){const el=$(`[data-param="${CSS.escape(p.name)}"]`,form);if(!el)continue;const s=p.schema||{};const label=String(p.name).replace(/_/g,' ');
    if(el.dataset.kind==='bool'){out[p.name]=el.getAttribute('aria-checked')==='true';continue}
    const raw=String(el.value||'').trim();if(!raw){if(p.required)throw new Error('Fill in '+label+' first.');continue}
    if(el.dataset.kind==='int'){const n=Number(raw);if(!Number.isInteger(n)||(s.min!=null&&n<s.min)||(s.max!=null&&n>s.max))throw new Error(sentence(label)+' must be a whole number'+(s.min!=null&&s.max!=null?' from '+s.min+' to '+s.max:'')+'.');out[p.name]=n}
    else out[p.name]=raw}
  return out}
/* a page for one device. `lead` draws the line at the top for a class that knows a little more than its fields */
function deviceSheet(t,opts,lead){
  opts=opts||{};const d=DEVINFO[t];if(!d&&!LAST[t])return {title:'Device',html:'<p class="quiet">That device is not on the list any more.</p>'};
  const name=nameOf(t);const cls=classOf(t);const card=cardOf(cls);
  const word=()=>lead?lead.word():deviceWord(t);const why=()=>lead&&lead.why?lead.why():(!usable(t)?'':stateOf(t)?'':reasonOf(t)&&reasonOf(t)!==deviceWord(t)?reasonOf(t):'');
  /* the readings, once there are some: until then the line at the top says why there are none */
  const fields=()=>{const st=stateOf(t);if(!st)return '<p class="quiet">None yet.</p>';const r=readings(st);return r.length?r.map(([k,v])=>row(null,esc(k),null,esc(v))).join(''):'<p class="quiet">It reports no values.</p>'};
  const acts=actionsFor(t);
  const html=`${failHtml(t)}
    <div class="rowi">${ICON(iconOf(t))}<div class="t"><b id="dv-word">${esc(word())}</b><small id="dv-why">${esc(why())}</small></div><span id="dv-pend">${PENDING[t]?PEND:''}</span></div>
    ${lead&&lead.html?lead.html():''}
    <h3 class="roomhead" style="font-size:17px">Readings</h3><div class="rows" id="dv-fields">${fields()}</div>
    ${acts.length?`<h3 class="roomhead" style="font-size:17px">Actions</h3><div class="actforms">${acts.map(actionForm).join('')}</div>`:''}
    ${notesBlock(t)}
    ${placeRow(t)}
    <details class="disc"><summary>Details<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></summary><dl class="facts"><dt>Kind</dt><dd>${esc(cls?sentence(cls):card.one)}</dd>${d&&d.instance?`<dt>Integration</dt><dd>${esc(d.instance)}${pluginLabel(t)?' · '+esc(pluginLabel(t)):''}</dd>`:''}${isOwner()?`<dt>Id</dt><dd><code>${esc(t)}</code></dd>`:''}</dl></details>`;
  return {title:name,html,glowHue:270,glowA:0.08,
    mount(root){
      $$('[data-actform]',root).forEach(form=>{const def=acts.find(x=>x.id===form.dataset.actform);if(!def)return;
        $$('[data-kind="bool"]',form).forEach(b=>b.onclick=()=>b.setAttribute('aria-checked',String(b.getAttribute('aria-checked')!=='true')));
        $('[data-run]',form).onclick=()=>{let params;try{params=formValues(form,def)}catch(e){toast(e.message,'');return}
          const a=ACT.any(def.id,t,params);const out=$('[data-result]',form);out.innerHTML='';
          a.onDone=(rt,v)=>{if(v&&v.pairing&&typeof v.pairing==='object'){const text=Object.entries(v.pairing).map(([k,x])=>k+': '+x).join('\n');out.innerHTML=`<p class="p2">Shown only now.</p><div class="copyrow"><code class="code">${esc(text)}</code><button class="btn" data-copy>Copy</button></div>`;$('[data-copy]',out).onclick=e=>copyText(text,e.currentTarget,$('code',out))}};
          armGuard(a,$('[data-acts]',form),{channel:'touch'})}});
      wireNotes(root);
      if(opts.action){const f=$$('[data-actform]',root).find(x=>x.dataset.actform===opts.action.action);if(f)armGuard(opts.action,$('[data-acts]',f),{channel:opts.channel,transcript:opts.transcript})}},
    update(){const root=$('#sbody');const w=$('#dv-word',root);if(!w)return;w.textContent=word();$('#dv-why',root).textContent=why();$('#dv-pend',root).innerHTML=PENDING[t]?PEND:'';const f=$('#dv-fields',root);if(f){const h=fields();if(f.dataset.h!==h){f.dataset.h=h;f.innerHTML=h}}}};
}
/* on Home, a device of a class with no card of its own is a small capsule that opens this page */
function genericTile(t){const w=stateOf(t)?deviceWord(t):usable(t)?'not reporting':'not available';return `<button class="g cap press" data-sheet="device:${esc(t)}" id="c-${esc(t)}">${ICON(iconOf(t))}<span class="lbl">${esc(nameOf(t))}</span><span class="st num">${esc(w)}</span></button>`}

const genericSheets={
  device(t,opts){return deviceSheet(t,opts)},
};

export {actionsFor,deviceSheet,genericTile,genericSheets};
