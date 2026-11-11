import {$,$$,pad,esc,store,ICON} from '../core/util.js';
import {VMS,DEVINFO,HEALTHDATA,TGT,vmById,isOwner,NR,T,usable,faultSection,crashCount} from '../core/store.js';
import {ACT} from '../core/catalog.js';
import {armGuard} from '../core/guards.js';
import {row,refreshSheet} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {chartGet,lastOf,chartHtml,wireChart,paintCharts,jumpTo,rangeHtml,wireRange,healthHtml,coreSummary,coreLegendInner,drawCores,wireCores} from '../pages/monitor.js';
import {notesBlock,wireNotes} from './index.js';
import {ramHtml,energyHtml,wireEnergy,paintUsage} from './hostusage.js';

/* ============================================================ a host: the machine the computers run on. Its name, its readings and its notes come from the hub */
let TE_METRIC=(()=>{const m=store.get('host-metric','load');return ['load','pkgW','pkgC'].includes(m)?m:'load'})();
const hostSheets={
  /* what you can do first, then health, then the readings. Restart and Shut down sit at the top, behind a passkey and a
     hold, so nothing needs a long scroll. When the host is off there are no readings at all. */
  host(arg,opts){opts=opts||{};
    const t=arg||TGT.host;const name=T.hostName;
    if(!t)return {title:'Host',html:'<p class="quiet">There is no host here yet.</p>'};
    if(t!==TGT.host)return {title:name,html:`<p class="quiet">Only the first host has a page of its own here.</p>`};
    const on=T.hostKnown&&T.hostUp;const off=T.hostKnown&&!T.hostUp;const up=Date.now()-T.bootAt;const upH=Math.floor(up/3600e3),upM=Math.floor(up/60e3)%60;
    const wg=TGT.router;const wgFree=!!wg&&usable(wg);
    const acts=isOwner()?`<div class="acts" id="te-acts"><button class="btn" data-host="reboot" ${off?'disabled':''}>Restart ${esc(name)}</button><button class="btn danger" data-host="shutdown" ${off?'disabled':''}>Shut down ${esc(name)}</button>${wg?`<button class="btn" data-host="wg" ${off||!wgFree?'disabled':''}>Restart remote access</button>`:''}</div><p class="p2">Each needs your passkey and a press-and-hold. There is no root shell for the host here.${wg&&!wgFree?' '+esc((DEVINFO[wg]||{}).reason||''):''}</p>`:'';
    const can=HEALTHDATA&&HEALTHDATA.cpu_canary;const canOk=can&&can.status==='ok';
    const stat=(id,label)=>`<div class="stat"><small>${label}</small><b class="num" id="${id}">—</b></div>`;
    /* power and temperature are charted only when something collects them */
    const charts=[['load','Load','tr-load-chart','Load over time']].concat(TGT.telemetry?[['pkgW','Power','te-pw','Package power over time'],['pkgC','Temp','te-pc','Package temperature over time']]:[]);
    if(!charts.some(c=>c[0]===TE_METRIC))TE_METRIC='load';
    const readings=off?`<div class="failband">${ICON('warn')}<span>Not reporting. ${esc(name)} is off, so there are no readings: no load, power, memory, storage or network.</span></div>`:`<div class="stats">${stat('tr-load','Load')}${TGT.telemetry?stat('te-w','Power')+stat('te-c','Temperature'):''}${stat('tr-mem','Memory')}</div>
      ${T.cores?`<div class="chartwrap"><canvas class="coremap" id="te-core" role="img" aria-label="${esc(coreSummary())}"></canvas><div class="tip num" hidden></div></div><div id="te-core-legend">${coreLegendInner()}</div>`:''}
      ${ramHtml()}
      ${energyHtml()}`;
    const history=off?'':`<h3 class="roomhead" style="font-size:17px">History</h3>
      <div class="chartbar">${charts.length>1?`<div class="segc" role="group" aria-label="Which reading">${charts.map(([k,l])=>`<button data-metric="${k}" aria-pressed="${TE_METRIC===k}">${l}</button>`).join('')}</div>`:''}${rangeHtml()}</div>
      ${charts.map(([k,,id,label])=>`<div data-metric-chart="${k}" ${TE_METRIC===k?'':'hidden'}>${chartHtml(id,label)}</div>`).join('')}`;
    const crashes=faultSection()?crashCount('host'):null;const healthWord=crashes==null?NR:crashes?crashes+' crash'+(crashes===1?'':'es'):'No crashes';
    const fold=(id,label,word,body,open)=>`<details class="disc" id="${id}" ${open?'open':''}><summary><span>${label}</span><span style="display:flex;align-items:center;gap:10px;color:var(--text-2);font-weight:400">${word?`<span class="num">${esc(word)}</span>`:''}<svg class="ic chev" aria-hidden="true"><use href="#i-chev"/></svg></span></summary>${body}</details>`;
    const coreTest=`<div class="rowi">${ICON('warn')}<div class="t"><b>Core test</b><small class="${canOk?'':'p2'}" id="te-can">${esc(canOk?(can.result!=null?String(can.result):'Reported'):NR)}</small><small class="fine">The only check that catches silent miscomputes.</small></div></div>`;
    const more=`<div class="rows">${row(null,'Space used','How full each drive is',NR)}</div>
      ${wg?`<div class="rows">${row(null,'Remote access',esc(T.routerName),'<span id="te-wg"></span>',`data-open="router:${esc(wg)}"`)}</div>`:''}`;
    const lead=T.hostKnown?(on?`Up ${upH} h ${pad(upM)} m.`:`${name} is off. It cannot be powered on from this page; someone at home has to press its button.`):`The computers below run on it. Not reporting.`;
    const vmRow=v=>row(v.icon,esc(v.name),v.pending?'Waiting for your approval':`<span class="fine">${esc(v.tech)}</span>`,`<span data-vmst="${esc(v.id)}"></span>`,`data-open="comp:${esc(v.id)}"`);
    const html=`<p class="p2">${esc(lead)}</p>
      ${readings}
      ${VMS.length?`<h3 class="roomhead" style="font-size:17px">Computers</h3>
      <div class="rows vmlist">${VMS.map(vmRow).join('')}</div>`:'<div class="vmlist" hidden></div>'}
      ${history}
      ${fold('te-health','Health',healthWord,healthHtml('host')+coreTest,opts.jump==='health'||crashes>0)}
      ${off?'':fold('te-more','Storage and remote access',null,more,false)}
      ${notesBlock(t)}
      ${placeRow(t)}
      ${isOwner()?fold('te-power','Restart and shut down',null,acts,!!opts.action):''}`;
    return {title:name,html,
      mount(root){root.dataset.rep=String(T.hostKnown)+String(on);$$('[data-host]',root).forEach(b=>b.onclick=()=>{const k=b.dataset.host;armGuard(k==='wg'?ACT.wg():ACT.host(k==='reboot'?'reboot':'shutdown'),$('#te-acts',root),{channel:'touch'})});if(opts.action&&$('#te-acts',root))armGuard(opts.action,$('#te-acts',root),{channel:opts.channel,transcript:opts.transcript});
        wireRange(root);
        wireChart($('#tr-load-chart',root),()=>chartGet('load',{min:0,max:100,fmt:x=>Math.round(x)+'%'}));
        wireChart($('#te-pw',root),()=>chartGet('pkgW',{min:0,fit:true,fmt:x=>Math.round(x)+' W'}));
        wireChart($('#te-pc',root),()=>chartGet('pkgC',{min:30,max:105,limit:100,limitLabel:'100 °C limit',fmt:x=>Math.round(x)+' °C'}));
        $$('[data-metric]',root).forEach(b=>b.onclick=()=>{TE_METRIC=b.dataset.metric;store.set('host-metric',TE_METRIC);$$('[data-metric]',root).forEach(x=>x.setAttribute('aria-pressed',String(x.dataset.metric===TE_METRIC)));$$('[data-metric-chart]',root).forEach(x=>x.hidden=x.dataset.metricChart!==TE_METRIC);paintCharts(root)});
        wireCores($('#te-core',root));wireEnergy(root);wireNotes(root);
        this.update();jumpTo(root,opts.jump==='health'?'#hl-host':null)},
      update(){const root=$('#sbody');if(!$('.vmlist',root))return;
        if(root.dataset.rep!==String(T.hostKnown)+String(T.hostKnown&&T.hostUp)&&!$('[data-guard]',root)){refreshSheet();return}
        $$('[data-vmst]',root).forEach(el=>{const v=vmById(el.dataset.vmst);if(!v)return;el.textContent=v.pending?'Not approved':v.transition?v.transition[0].toUpperCase()+v.transition.slice(1)+'…':!v.known?NR:v.state==='running'?'On':'Off'});
        const l=T.cpu!=null?T.cpu:lastOf('load');const ld=$('#tr-load',root);if(ld)ld.textContent=l==null?'—':Math.round(l)+'%';
        const mem=$('#tr-mem',root);if(mem)mem.textContent=T.memUsed==null?'—':(T.memUsed/1073741824).toFixed(1)+' GB';
        const tw=$('#te-w',root);if(tw){tw.textContent=T.pkgW==null?'—':T.pkgW.toFixed(0)+' W';$('#te-c',root).textContent=T.pkgC==null?'—':T.pkgC.toFixed(0)+' °C'}
        const wgEl=$('#te-wg',root);if(wgEl)wgEl.textContent=T.wgKnown?(T.wgOk?'Up':'Down'):NR;
        const cv=$('#te-core',root);if(cv)cv.setAttribute('aria-label',coreSummary());const key=$('#te-core-legend',root);if(key)key.innerHTML=coreLegendInner();
        paintUsage(root);paintCharts(root);drawCores(cv)}};
  },
  technical(arg,opts){return hostSheets.host('',opts)},
};

export {hostSheets};
