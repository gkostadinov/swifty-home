import {$,$$,esc,ICON,PEND} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {isOwner} from '../core/store.js';
import {toast} from '../ui/toast.js';
import {onEvent,eventsOn} from '../core/events.js';
import {SHEET,openSheet,closeSheet} from '../ui/sheets.js';
import {afterChange,pluginName} from './integrations.js';

/* ============================================================ setup: a plugin's guided steps, drawn from data. A plugin ships no page and no script of its own */
/* six kinds of step: form, wait, choose, external, done and failed. The same steps reach `swifty plugin add` in a terminal */
const SETUP={id:null,step:null,last:null,retry:null,meta:{},busy:false,back:false,count:0,entered:{},waitKey:'',waitUntil:0,pollT:0,tickT:0,off:null,ended:false};
/* the hub answers with the whole view: the step to draw, and with a failure that can be tried again, the step to draw once more (`retry`) */
const stepOf=r=>r&&r.step&&typeof r.step==='object'?r.step:r;
const retryOf=r=>r&&r.retry&&typeof r.retry==='object'?r.retry:null;
const sheetName=()=>'setup:'+SETUP.id;
const repaint=()=>{if(SHEET.open&&SHEET.type===sheetName())openSheet(sheetName(),null,{replace:true,focus:'keep'})};
function stopTimers(){clearTimeout(SETUP.pollT);clearInterval(SETUP.tickT);SETUP.pollT=0;SETUP.tickT=0}
/* the hub opened a setup for this person: show its first step */
async function startSetup(id,step,meta){
  stopTimers();if(SETUP.off)SETUP.off();
  Object.assign(SETUP,{id,step:null,last:null,retry:null,meta:meta||{},busy:false,back:false,count:0,entered:{},waitKey:'',waitUntil:0,ended:false});
  SETUP.off=onEvent('setup',d=>{if(d&&d.setup_id===SETUP.id&&!SETUP.ended)fetchStep()});
  openSheet('setup:'+id,null,{replace:SHEET.open});
  if(step&&typeof step==='object'&&step.step)show(step);else await fetchStep();
}
/* each new step replaces the page. A failure keeps the step before it, so Try again can offer it once more */
const stepKey=st=>st?st.step+'|'+(st.title||''):'';
function show(st,retry){if(!st||!st.step){st={step:'failed',reason:'The hub sent a step this page does not know.',retry:false}}
  /* the step number follows the steps as they come: on to a new one, or back to the one before */
  if(st.step!=='failed'&&st.step!=='done'&&SETUP.last&&stepKey(st)!==stepKey(SETUP.last))SETUP.count=Math.max(0,SETUP.count+(SETUP.back?-1:1));SETUP.back=false;
  SETUP.step=st;SETUP.retry=st.step==='failed'&&st.retry?retry||null:null;if(st.step!=='failed'&&st.step!=='done')SETUP.last=st;
  if(st.step==='done'||(st.step==='failed'&&!st.retry))SETUP.ended=true;
  if(st.step==='done')afterChange().catch(()=>{});
  /* a wait that comes back after something else (a try again after it ran out) counts from the start again, as the hub's clock does */
  if(st.step!=='wait'){stopTimers();SETUP.waitKey=''}
  repaint()}
async function fetchStep(){if(!SETUP.id||SETUP.ended)return;try{const r=await api('GET','/setup/'+enc(SETUP.id),undefined,{quiet:true});const st=stepOf(r);if(JSON.stringify(st)!==JSON.stringify(SETUP.step))show(st,retryOf(r))}catch(e){gone(e)}}
function gone(e){if(e&&(e.status===404||e.status===410||e.status===403)){SETUP.ended=true;show({step:'failed',reason:e.message||'This setup has ended.',retry:false})}else if(e)toast(e.message,'fail')}
async function send(input){if(SETUP.busy||SETUP.ended)return;SETUP.busy=true;$$('#sbody [data-sbtn]').forEach(b=>b.disabled=true);
  try{const r=await api('POST','/setup/'+enc(SETUP.id),input,{quiet:true});SETUP.busy=false;SETUP.back=input.input==='back';show(stepOf(r),retryOf(r))}
  /* the hub refused the input and still holds the step it answered: that is the one to offer again */
  catch(e){SETUP.busy=false;if(e.status===404||e.status===410||e.status===403)gone(e);else show({step:'failed',reason:e.message,retry:true},SETUP.last)}}
/* leaving a setup that is not over lets the hub drop it: nothing of it is kept */
function abandon(){stopTimers();if(SETUP.off){SETUP.off();SETUP.off=null}const id=SETUP.id;if(id&&!SETUP.ended){SETUP.ended=true;api('DELETE','/setup/'+enc(id),undefined,{quiet:true}).catch(()=>{})}}

/* ---------- one field of a form, by its type. A secret is masked, never filled in and never kept ---------- */
const HINT={host:'Such as 192.0.2.20',host_port:'Such as 192.0.2.20:8080',mac:'Such as aa:bb:cc:dd:ee:ff',url:'The full address, with its scheme'};
function fieldHtml(f,i){const k=f.kind||{};const t=k.type||'text';const key=esc(f.key);const id='sf-'+i;const nm=esc(f.label||f.key);
  const label=`<span class="sl">${nm}${f.required?'':' <span class="fine">optional</span>'}</span>`;const help=f.help?`<small>${esc(f.help)}</small>`:'';
  const pre=t==='secret'?'':SETUP.entered[f.key]!=null?SETUP.entered[f.key]:(f.value!=null?f.value:'');
  if(t==='bool')return `<div class="rowi sfield"><div class="t"><b>${nm}</b>${help}</div><button class="toggle" role="switch" aria-checked="${pre===true}" aria-label="${nm}" data-sf="${key}" data-t="bool" id="${id}"></button></div>`;
  if(t==='choice'){const opts=k.options||[];
    if(opts.length&&opts.length<=4&&opts.every(o=>String(o).length<=14))return `<div class="sfield">${label}<div class="segc" role="group" aria-label="${nm}" data-sf="${key}" data-t="choice" data-seg="1" id="${id}">${opts.map(o=>`<button type="button" aria-pressed="${String(pre)===String(o)}" data-v="${esc(o)}">${esc(o)}</button>`).join('')}</div>${help}</div>`;
    return `<label class="sfield">${label}<select class="field" data-sf="${key}" data-t="choice" id="${id}">${f.required?'':'<option value=""></option>'}${opts.map(o=>`<option value="${esc(o)}" ${String(pre)===String(o)?'selected':''}>${esc(o)}</option>`).join('')}</select>${help}</label>`}
  if(t==='secret')return `<div class="sfield"><label for="${id}">${label}</label><div class="inline-edit"><input class="field masked" type="text" id="${id}" data-sf="${key}" data-t="secret" autocomplete="off" autocapitalize="off" spellcheck="false" ${SETUP.meta.again?'placeholder="Leave empty to keep the stored one"':''}><button type="button" class="pill" data-unmask="${id}" aria-pressed="false">Show</button></div>${help}</div>`;
  if(t==='multiline')return `<label class="sfield">${label}<textarea class="field area" rows="5" id="${id}" data-sf="${key}" data-t="multiline" ${k.max_len?`maxlength="${esc(k.max_len)}"`:''} autocomplete="off" spellcheck="false">${esc(pre)}</textarea>${help}</label>`;
  const num=t==='port'||t==='int';const lo=t==='port'?1:k.min,hi=t==='port'?65535:k.max;
  return `<label class="sfield">${label}<input class="field" id="${id}" data-sf="${key}" data-t="${esc(t)}" type="${num?'number':'text'}" ${num?`inputmode="numeric" step="1" ${lo!=null?`min="${esc(lo)}"`:''} ${hi!=null?`max="${esc(hi)}"`:''}`:t==='url'?'inputmode="url"':''} ${t==='text'&&k.max_len?`maxlength="${esc(k.max_len)}"`:''} ${HINT[t]?`placeholder="${esc(HINT[t])}"`:''} value="${esc(pre)}" autocomplete="off" autocapitalize="off" spellcheck="false">${help}</label>`}
/* the values of a form, checked as the hub will check them. Secrets are read once and the boxes emptied at once */
function collect(root,fields){const values={},entered={};let bad=null;
  for(const f of fields){const el=$(`[data-sf="${CSS.escape(f.key)}"]`,root);if(!el)continue;const t=el.dataset.t;const nm=f.label||f.key;el.removeAttribute('aria-invalid');
    const fail=msg=>{if(!bad){bad={el,msg}}el.setAttribute('aria-invalid','true')};
    if(t==='bool'){values[f.key]=el.getAttribute('aria-checked')==='true';entered[f.key]=values[f.key];continue}
    let raw=el.dataset.seg?(($('[aria-pressed="true"]',el)||{}).dataset||{}).v||'':String(el.value||'');if(t!=='multiline'&&t!=='secret')raw=raw.trim();
    if(!raw){if(f.required&&!(t==='secret'&&SETUP.meta.again))fail('Fill in '+nm+'.');continue}
    if(t==='port'||t==='int'){const k=f.kind||{};const lo=t==='port'?1:k.min,hi=t==='port'?65535:k.max;if(!/^-?\d+$/.test(raw)){fail(nm+' must be a whole number.');continue}const n=Number(raw);if((lo!=null&&n<lo)||(hi!=null&&n>hi)){fail(nm+' must be from '+lo+' to '+hi+'.');continue}values[f.key]=n}
    else values[f.key]=raw;
    if(t!=='secret')entered[f.key]=values[f.key]}
  for(const el of $$('[data-t="secret"]',root))el.value='';
  if(bad){toast(bad.msg,'');try{bad.el.focus()}catch(e){}return null}
  return {values,entered}}
const linkOk=u=>{try{return ['https:','http:'].includes(new URL(u).protocol)}catch(e){return false}};
function fmtLeft(ms){const s=Math.max(0,Math.ceil(ms/1000));return Math.floor(s/60)+':'+String(s%60).padStart(2,'0')}

const setupSheets={
  setup(id){
    if(!isOwner())return {title:'Setup',html:'<p class="quiet">Only the owner can set up integrations.</p>'};
    if(SETUP.id!==id)return {title:'Setup',html:'<p class="quiet">That setup has ended. Start it again from Integrations.</p><div class="acts"><button class="btn" data-open="integrations">Integrations</button></div>'};
    const st=SETUP.step;const name=pluginName(SETUP.meta.plugin)||'Setup';
    const head=`<div class="wizhead">${ICON('puzzle')}<span>${esc(name)}${SETUP.meta.again?' · set up again':''}</span>${st&&st.step!=='done'&&st.step!=='failed'?`<span class="fine">Step ${SETUP.count+1}</span>`:''}</div>`;
    const back=SETUP.count>0?'<button class="btn" data-sbtn data-back>Back</button>':'';
    const cancel='<button class="btn quiet" data-sbtn data-cancel>Cancel</button>';
    let body='',title=name;
    if(!st)body=`<p class="p2">${PEND} Asking the hub for the first step…</p>`;
    else switch(st.step){
      case 'form':case 'external':{const fields=st.fields||[];title=st.title||name;
        body=`<p class="p2">${esc(st.text||'')}</p>
          ${st.step==='external'&&st.link&&linkOk(st.link)?`<div class="acts"><a class="btn" href="${esc(st.link)}" target="_blank" rel="noopener noreferrer">${ICON('out')}Open that page</a></div><p class="fine">It opens in a new tab. Come back here when you have what it asks for.</p>`:''}
          <div class="sform">${fields.map(fieldHtml).join('')}</div>
          <div class="acts">${back}<button class="btn primary" data-sbtn data-next>Continue</button></div>${cancel}`;break}
      case 'wait':{title=st.title||name;
        body=`<p class="sent">${esc(st.text||'')}</p>
          <div class="waitbox"><span class="pend" aria-hidden="true"></span><span>Waiting<span class="num" id="sw-left"></span></span></div>
          <p class="p2">This page goes on by itself.</p>
          <div class="acts"><button class="btn" data-sbtn data-back>Give up and go back</button></div>${cancel}`;break}
      case 'choose':{const opts=st.options||[];title=st.title||name;const multi=!!st.multiple;
        body=`<p class="p2">${esc(st.text||'')}</p>
          ${opts.length?`<div class="rows choices" role="${multi?'group':'radiogroup'}" aria-label="${esc(st.title||'Choices')}">${opts.map(o=>`<div class="rowi"><div class="t"><b>${esc(o.label||o.id)}</b>${o.detail?`<small>${esc(o.detail)}</small>`:''}</div><button class="toggle" role="${multi?'switch':'radio'}" aria-checked="${!!o.selected}" aria-label="${esc(o.label||o.id)}" data-ch="${esc(o.id)}"></button></div>`).join('')}</div>`:'<p class="quiet">Nothing was found to pick from.</p>'}
          ${multi&&opts.length>3?'<div class="acts"><button class="pill" data-chall>Pick all</button><button class="pill" data-chnone>Pick none</button></div>':''}
          ${st.min?`<p class="fine">Pick at least ${esc(st.min)}.</p>`:''}
          <div class="acts">${back}<button class="btn primary" data-sbtn data-next>Continue</button></div>${cancel}`;break}
      case 'done':title='Done';
        body=`<div class="okline">${ICON('check')}<span>${esc(st.summary||'Set up.')}</span></div>
          <p class="p2">Its devices show up on their own, on Home and under Devices.</p>
          <div class="acts"><button class="btn primary" data-finish>Done</button><button class="btn" data-open="integrations">Integrations</button></div>`;break;
      case 'failed':title='It did not work';
        body=`<div class="failband">${ICON('warn')}<span>${esc(st.reason||'The setup stopped.')}</span></div>
          <div class="acts">${st.retry&&SETUP.retry?'<button class="btn primary" data-retry-step>Try again</button>':''}<button class="btn" data-finish>Close</button></div>`;break;
      default:body=`<p class="quiet">The hub sent a step this page does not know: ${esc(st.step)}.</p>${cancel}`;
    }
    return {title,html:head+body,glowHue:270,glowA:0.1,
      mount(root){
        $$('[data-unmask]',root).forEach(b=>b.onclick=()=>{const inp=$('#'+b.dataset.unmask,root);const show=inp.classList.toggle('masked')===false;b.textContent=show?'Hide':'Show';b.setAttribute('aria-pressed',String(show))});
        $$('[data-t="bool"]',root).forEach(b=>b.onclick=()=>b.setAttribute('aria-checked',String(b.getAttribute('aria-checked')!=='true')));
        $$('[data-seg]',root).forEach(g=>$$('[data-v]',g).forEach(b=>b.onclick=()=>{$$('[data-v]',g).forEach(x=>x.setAttribute('aria-pressed',String(x===b)))}));
        const multi=st&&st.step==='choose'&&!!st.multiple;
        $$('[data-ch]',root).forEach(b=>b.onclick=()=>{const on=b.getAttribute('aria-checked')!=='true';if(!multi)$$('[data-ch]',root).forEach(x=>x.setAttribute('aria-checked','false'));b.setAttribute('aria-checked',String(multi?on:true))});
        const all=$('[data-chall]',root);if(all)all.onclick=()=>$$('[data-ch]',root).forEach(x=>x.setAttribute('aria-checked','true'));
        const none=$('[data-chnone]',root);if(none)none.onclick=()=>$$('[data-ch]',root).forEach(x=>x.setAttribute('aria-checked','false'));
        const next=$('[data-next]',root);if(next)next.onclick=()=>{
          if(st.step==='choose'){const ids=$$('[data-ch]',root).filter(x=>x.getAttribute('aria-checked')==='true').map(x=>x.dataset.ch);if(ids.length<(st.min||0)){toast('Pick at least '+st.min+'.','');return}send({input:'chosen',ids});return}
          const got=collect(root,st.fields||[]);if(!got)return;Object.assign(SETUP.entered,got.entered);send({input:'values',values:got.values})};
        $$('[data-back]',root).forEach(b=>b.onclick=()=>send({input:'back'}));
        $$('[data-cancel]',root).forEach(b=>b.onclick=()=>{abandon();closeSheet();toast('Setup stopped. Nothing was kept.','')});
        $$('[data-finish]',root).forEach(b=>b.onclick=()=>{if(st&&st.step==='done')toast(st.summary||'Set up.','ok');closeSheet()});
        /* a wait that ran out is asked again with a poll, as the hub offers it; any other step is drawn again, with what was typed */
        const rt=$('[data-retry-step]',root);if(rt)rt.onclick=()=>{const again=SETUP.retry;if(!again)return;if(again.step==='wait'){send({input:'poll'});return}SETUP.step=again;SETUP.retry=null;repaint()};
        /* a wait counts down, and asks the hub again when it says something moved on, or from time to time when the stream is down */
        if(st&&st.step==='wait'){const key=(st.title||'')+'|'+(st.text||'');if(SETUP.waitKey!==key){SETUP.waitKey=key;SETUP.waitUntil=Date.now()+(+st.timeout_s||120)*1000}
          const left=$('#sw-left',root);const tick=()=>{if(left&&left.isConnected)left.textContent=' · '+fmtLeft(SETUP.waitUntil-Date.now())+' left'};tick();clearInterval(SETUP.tickT);SETUP.tickT=setInterval(tick,1000);
          const poll=()=>{clearTimeout(SETUP.pollT);SETUP.pollT=setTimeout(async()=>{if(SETUP.ended||!SETUP.step||SETUP.step.step!=='wait')return;await fetchStep();poll()},eventsOn()?5000:Math.max(1500,+st.poll_ms||2000))};poll()}
        else{const f=$('.sform input,.sform textarea,.sform select',root);if(f)setTimeout(()=>{try{f.focus({preventScroll:true})}catch(e){}},120)}},
      close(){abandon()}};
  },
};

export {startSetup,setupSheets};
