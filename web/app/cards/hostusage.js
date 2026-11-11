import {$,$$,esc} from '../core/util.js';
import {T,TGT,NR,ENERGY,isOwner,loadEnergy,saveEnergyTariff,energyOfTarget} from '../core/store.js';
import {memoryParts,gbText,kwhText,moneyText,priceFrom,currencyFrom} from '../core/hostdata.js';
import {row,fmtDay} from '../ui/sheets.js';
import {toast} from '../ui/toast.js';
import {guestName} from '../pages/monitor.js';

/* ============================================================ what the host uses: its memory split by who holds it, and the energy it has used and what that cost.
   Everything is what the collector reports and what the hub counted; a reading that is missing is shown as missing, never as zero */

/* the host's memory as one bar: each computer's share, the host's own use, and what is free. The colours are the ones of the threads chart */
function ramInner(){
  const m=memoryParts(T.memTotal,T.memUsed,T.guests);if(!m)return '';
  const pct=b=>(b/m.scale*100).toFixed(2)+'%';
  const bar=m.parts.map(p=>`<i style="width:${pct(p.bytes)};background:var(--core-${p.guest.slot})"></i>`).join('')+(m.host?`<i style="width:${pct(m.host)};background:var(--text-3);opacity:.4"></i>`:'');
  const say=g=>!g.running?'off':g.memUsed==null?NR.toLowerCase():g.memAssigned?gbText(g.memUsed)+' of '+gbText(g.memAssigned):gbText(g.memUsed);
  const key=T.guests.map(g=>`<span><i style="background:var(--core-${g.slot})"></i>${esc(guestName(g))} · ${esc(say(g))}</span>`).join('')
    +(m.host!=null?`<span><i style="background:var(--text-3);opacity:.4"></i>Host · ${gbText(m.host)}</span>`:'')
    +(m.free!=null?`<span><i style="background:var(--field);box-shadow:inset 0 0 0 1px var(--line)"></i>Free · ${gbText(m.free)}</span>`:'');
  const words='Memory by computer: '+m.parts.map(p=>guestName(p.guest)+' '+gbText(p.bytes)).join(', ')+(m.host!=null?', host '+gbText(m.host):'')+(m.free!=null?', free '+gbText(m.free):'')+', of '+gbText(m.total)+'.';
  return `<h3 class="roomhead" style="font-size:17px">Memory by computer</h3>
    <div class="stack" role="img" aria-label="${esc(words)}">${bar}</div>
    <div class="legend">${key}</div>
    <p class="fine">${gbText(m.total)} in the host. A computer's share is the host memory it takes.</p>`;
}
const ramHtml=()=>`<div id="te-ram">${ramInner()}</div>`;

/* the energy counter, and the cost the hub has counted for it at the owner's tariff */
const SCOPE={components:'CPU and GPU, not the whole machine at the wall',wall:'Measured at the wall'};
const hoursText=h=>h?h.start+' to '+h.end:'';
/* the tariff in words: one price, or day and night with the hours and which one applies now */
function tariffWords(t){
  if(t.night==null)return 'At '+t.day+' '+t.currency+' per kWh.';
  return 'Day '+t.day+', night '+t.night+' '+t.currency+' per kWh. Night is '+hoursText(t.hours)+(t.summer?', or '+hoursText(t.summer)+' in summer time':'')+'. Now: the '+t.now+' rate.';
}
function energyRead(){
  const e=T.energy;if(!e)return `<p class="p2">${NR}.</p>`;
  const led=energyOfTarget(TGT.telemetry);const t=ENERGY.tariff;
  const title=e.since!=null?'Energy since '+fmtDay(e.since):'Energy used';
  const domains=e.domains.length>1?e.domains.map(d=>row(null,esc(d.label),d.watts!=null?Math.round(d.watts*10)/10+' W now':null,kwhText(d.kwh))).join(''):'';
  let cost='';
  if(t){
    const priced=led&&led.cost!=null;const cur=esc(t.currency);
    const note=tariffWords(t)+' An estimate.'+(priced&&led.backfilled_kwh>0?' Includes '+kwhText(led.backfilled_kwh)+' used before the tariff was set, at the day price.':'');
    cost=row(null,'Cost',esc(priced?note:'Counted from the next reading.'),priced?moneyText(led.cost,cur):'—');
    if(priced&&t.night!=null)cost+=row(null,'By day',esc(kwhText(led.kwh_day)),moneyText(led.cost_day,cur))+row(null,'At night',esc(kwhText(led.kwh_night)),moneyText(led.cost_night,cur));
  }
  return `<div class="rows">${row(null,esc(title),esc(SCOPE[e.scope]||''),kwhText(e.kwh))}${domains}${cost}</div>`;
}
/* the time zone the night hours are read in: the tariff's own once there is one, else this device's */
const ZONE=(()=>{try{return Intl.DateTimeFormat().resolvedOptions().timeZone||'UTC'}catch(e){return 'UTC'}})();
/* one price all day until night is cheaper; then the first price is the day price */
const dayLabel=two=>two?'Day price per kWh':'Price per kWh';
const sw=(id,on,label)=>`<button class="toggle" role="switch" id="${id}" aria-checked="${on}" aria-label="${esc(label)}"></button>`;
/* a labelled field, as the setup wizard draws them: the label above, in the text's own colour */
const field=(id,label,attrs,labelId)=>`<label class="sfield" for="${id}"><span class="sl"${labelId?` id="${labelId}"`:''}>${label}</span><input class="field" id="${id}" ${attrs}></label>`;
const timeField=(id,label,value)=>`<input class="field" type="time" id="${id}" aria-label="${esc(label)}" value="${esc(value)}">`;
/* night hours: one label over "from to" */
const hoursField=(label,a,b,h)=>`<div class="sfield"><span class="sl">${label}</span><div class="times">${timeField(a,label+', from',h.start)}<span class="to">to</span>${timeField(b,label+', to',h.end)}</div></div>`;
/* the owner's tariff: a price per kWh and a currency code, and for a two-rate meter a night price with its hours.
   It is kept apart from the readings so a refresh never clears what is being typed */
function energyEdit(){
  const t=ENERGY.tariff;const two=!!(t&&t.night!=null);const summer=!!(t&&t.summer);
  const h=(t&&t.hours)||{start:'22:00',end:'06:00'};const s=(t&&t.summer)||{start:'23:00',end:'07:00'};
  const price=v=>`inputmode="decimal" autocomplete="off" value="${v==null?'':esc(v)}"`;
  return `<div class="tariff">
    <h3 class="roomhead" style="font-size:17px">Electricity tariff</h3>
    <div class="tariff-price">
      ${field('en-day',dayLabel(two),price(t&&t.day)+' placeholder="e.g. 0.31"','en-daylabel')}
      ${field('en-cur','Currency',`data-cur autocomplete="off" maxlength="3" placeholder="e.g. EUR" value="${t?esc(t.currency):''}"`)}
    </div>
    <div class="rowi"><div class="t"><b>Cheaper at night</b><small>A two-rate meter: another price at night.</small></div>${sw('en-two',two,'Cheaper at night')}</div>
    <div class="tariff-group" id="en-nightbox"${two?'':' hidden'}>
      ${field('en-night','Night price per kWh',price(two?t.night:null)+' placeholder="e.g. 0.15"')}
      ${hoursField('Night hours','en-ns','en-ne',h)}
      <div class="rowi"><div class="t"><b>Different hours in summer time</b><small>While the clocks are on summer time.</small></div>${sw('en-summer',summer,'Different hours in summer time')}</div>
      <div class="tariff-group" id="en-summerbox"${summer?'':' hidden'}>${hoursField('Night hours in summer time','en-ss','en-se',s)}</div>
      <small class="fine">Hours are in ${esc((t&&t.zone)||ZONE)} time.</small>
    </div>
    <div class="tariff-save"><button class="btn primary" id="en-save">Save</button>
      <small class="fine">${t?'A change counts from now. Costs already counted stay as they were.':'Energy used before you save is counted once at the day price.'}</small></div>
  </div>`;
}
/* what the form shows, so a refresh repaints it only when the tariff itself changed */
const tariffKey=()=>{const t=ENERGY.tariff;return t?JSON.stringify([t.currency,t.day,t.night,t.hours,t.summer,t.zone]):''};
const energyHtml=()=>!TGT.telemetry?'':`<div id="te-energy"><h3 class="roomhead" style="font-size:17px">Energy</h3>
  <div id="te-energy-read">${energyRead()}</div>${isOwner()&&(T.energy||ENERGY.tariff)?`<div id="te-energy-edit" data-key="${esc(tariffKey())}">${energyEdit()}</div>`:''}</div>`;

/* the tariff the form describes, or the reason it is not one yet with the field to fix */
function tariffFrom(box){
  const day=priceFrom($('#en-day',box).value),cur=currencyFrom($('#en-cur',box).value);
  if(day==null)return {bad:'#en-day',why:'Enter a price above 0, such as 0.31.'};
  if(!cur)return {bad:'#en-cur',why:'Enter the currency as three letters, such as EUR.'};
  const body={currency:cur,day_price_per_kwh:day};
  if($('#en-two',box).getAttribute('aria-checked')!=='true')return {body};
  const night=priceFrom($('#en-night',box).value);
  if(night==null)return {bad:'#en-night',why:'Enter the night price, such as 0.15.'};
  const hours=(a,b)=>{const s=$(a,box).value,e=$(b,box).value;return s&&e&&s!==e?{start:s,end:e}:null};
  const nightHours=hours('#en-ns','#en-ne');
  if(!nightHours)return {bad:'#en-ns',why:'Night needs a start and an end that differ.'};
  Object.assign(body,{night_price_per_kwh:night,night:nightHours,time_zone:(ENERGY.tariff&&ENERGY.tariff.zone)||ZONE});
  if($('#en-summer',box).getAttribute('aria-checked')==='true'){
    const summerHours=hours('#en-ss','#en-se');
    if(!summerHours)return {bad:'#en-ss',why:'Summer night needs a start and an end that differ.'};
    body.summer_night=summerHours;
  }
  return {body};
}
function wireEnergy(root){
  const box=$('#te-energy-edit',root);if(!box)return;
  const save=$('#en-save',box);if(!save)return;
  const flip=(id,part,then)=>{const b=$(id,box);b.onclick=()=>{const on=b.getAttribute('aria-checked')!=='true';b.setAttribute('aria-checked',String(on));$(part,box).hidden=!on;if(then)then(on)}};
  flip('#en-two','#en-nightbox',two=>{$('#en-daylabel',box).textContent=dayLabel(two)});
  flip('#en-summer','#en-summerbox');
  save.onclick=async()=>{
    $$('input',box).forEach(el=>el.removeAttribute('aria-invalid'));
    const got=tariffFrom(box);
    if(!got.body){const el=$(got.bad,box);el.setAttribute('aria-invalid','true');toast(got.why,'');el.focus();return}
    save.disabled=true;
    try{await saveEnergyTariff(got.body);toast('Tariff saved.','ok');repaintEnergy(root,true)}catch(err){save.disabled=false;toast(err.message,'fail')}};
  $$('input',box).forEach(el=>el.addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();save.click()}}));
}
function repaintEnergy(root,edit){
  const read=$('#te-energy-read',root);if(read)read.innerHTML=energyRead();
  const box=$('#te-energy-edit',root);
  if(box&&(edit||(box.dataset.key!==tariffKey()&&!box.contains(document.activeElement)))){box.dataset.key=tariffKey();box.innerHTML=energyEdit();wireEnergy(root)}
}
/* the hub counts the cost about once a minute; while the page is open it asks again now and then */
let busy=false;
function refreshEnergy(root){
  if(busy||!isOwner()||!TGT.telemetry||Date.now()-ENERGY.at<30e3)return;
  busy=true;loadEnergy().finally(()=>{busy=false;const r=$('#sbody');if(r&&$('#te-energy-read',r))repaintEnergy(r,false)});
}
/* what a refresh of the host's readings repaints */
function paintUsage(root){
  const ram=$('#te-ram',root);if(ram)ram.innerHTML=ramInner();
  repaintEnergy(root,false);refreshEnergy(root);
}

export {ramHtml,energyHtml,wireEnergy,paintUsage};
