import {clamp,hhmm} from '../core/util.js';
import {VMS,ROOM_NAMES,escRe,roomByName,S,person,isOwner,D,T,TGT,devName} from '../core/store.js';
import {problems,status} from '../core/status.js';
import {bulbById,bulbName,roomBulbs,lightsSummary,AC_MODES_SHOWN} from '../pages/home.js';
import {toast} from '../ui/toast.js';
import {HUES,ACT} from '../core/catalog.js';
import {runScene} from '../ui/scenes.js';
import {dispatch,doNow} from '../core/guards.js';
import {SHEET,openSheet,closeSheet} from '../ui/sheets.js';
import {lock} from '../ui/lock.js';
import {enterWall} from '../ui/dock.js';
import {setView} from '../ui/views.js';

/* ============================================================ search and voice share one parser */
/* the TV remote's buttons, when the sentence names one: “pause the tv”, “tv home”, “press ok on the tv”, “next channel” */
function tvKeyOf(text){if(/\bchannel up\b|\bnext channel\b/.test(text))return ['channel_up','Channel up'];if(/\bchannel down\b|\bprevious channel\b/.test(text))return ['channel_down','Channel down'];
  if(/\bpause\b/.test(text))return ['pause','Pause'];if(/\b(play|resume)\b/.test(text))return ['play','Play'];
  const m=text.match(/\b(home|back|exit|ok|okay|enter|select)\b/);if(m)return {home:['home','Home'],back:['back','Back'],exit:['exit','Exit']}[m[1]]||['ok','OK'];
  const a=/\b(press|arrow|go|move)\b/.test(text)&&text.match(/\b(up|down|left|right)\b/);if(a)return [a[1],a[1][0].toUpperCase()+a[1].slice(1)];return null}
const AC_WORDS=/\b(ac|a\/c|aircon|air ?con|air conditioner|air conditioning|climate)\b/;
/* the device whose own name the sentence uses. The longest name wins, so “socket 1 usb” is never “socket 1” */
function namedDevice(text){let best=null;const consider=(kind,d,name)=>{name=said(name);if(name.length<3||(best&&best.name.length>=name.length))return;if(new RegExp('(^|[^a-z0-9])'+escRe(name)+'($|[^a-z0-9])').test(text))best={kind,d,name}};
  D.plugs.forEach(p=>consider('plug',p,p.name));D.acs.forEach(c=>consider('ac',c,c.name));D.bulbs.forEach(b=>consider('bulb',b,bulbName(b)));return best}
/* “set the ac to 22”, “ac to cool”, “bedroom ac fan high”, “ac off”. A setting wakes it first when it is off */
function acCommand(c,rest){const t=c.target;if(/\boff\b/.test(rest))return {kind:'action',a:ACT.acPower(t,false)};const on=/\bon\b/.test(rest);
  const fm=rest.match(/\bfan(?:\s+speed)?\s*(?:to\s+|on\s+|at\s+)?(auto|quiet|silent|low|medium[ -]low|medium[ -]high|medium|mid|high|max)\b/);
  const fan=fm?({silent:'quiet',max:'high',mid:'medium'}[fm[1]]||fm[1].replace(/[ -]/,'_')):null;const r=fm?rest.replace(fm[0],' '):rest;
  const deg=r.match(/\b(\d{2})(?:[.,]\d)?\s*(?:°|degrees?|c\b)?/);let temp=deg?+deg[1]:null;
  if(temp==null&&c.temp!=null){if(/\b(warmer|hotter|higher)\b|\bup$/.test(r))temp=c.temp+1;else if(/\b(colder|cooler|lower)\b|\bdown$/.test(r))temp=c.temp-1}
  const mode=/\b(cool|cooling)\b/.test(r)?'cool':/\b(heat|heating)\b/.test(r)?'heat':/\b(dry|dehumidify)\b/.test(r)?'dry':/\bfan(?: only| mode)?\b/.test(r)?'fan':/\bauto(?:matic)?\b/.test(r)?'auto':null;
  const list=[];const asleep=!(c.known&&c.on);if(on||asleep&&(mode||temp!=null||fan))list.push(ACT.acPower(t,true));
  if(mode)list.push(ACT.acMode(t,mode));if(temp!=null)list.push(ACT.acTemp(t,temp));if(fan)list.push(ACT.acFan(t,fan));
  if(!list.length)return {kind:'query',answer:c.known?c.name+' is '+(c.on?'on'+(c.mode?', '+((AC_MODES_SHOWN.find(x=>x[0]===c.mode)||[0,c.mode])[1]).toLowerCase()+' mode':'')+(c.temp!=null?' at '+c.temp+'°':''):'off')+'.':c.name+' is not reporting.'};
  return list.length===1?{kind:'action',a:list[0]}:{kind:'multi',list,seq:true,label:c.name+': '+list.map(a=>a.doneText).join(', '),icon:'snow',k:''}}
/* what a recogniser writes, as the parser reads it. Names get the same digits and the same ac, so “lamp two” is still found in “lamp 2” */
const NUM_UNITS=['zero','one','two','three','four','five','six','seven','eight','nine','ten','eleven','twelve','thirteen','fourteen','fifteen','sixteen','seventeen','eighteen','nineteen'];
const NUM_TENS=['twenty','thirty','forty','fifty','sixty','seventy','eighty','ninety'];
const NUM_TENS_RE=new RegExp('\\b('+NUM_TENS.join('|')+')(?:[- ]('+NUM_UNITS.slice(1,10).join('|')+'))?\\b','g'),NUM_UNITS_RE=new RegExp('\\b('+NUM_UNITS.join('|')+')\\b','g');
function digitsOf(t){return t.replace(/(^|\s)a[.\/]?\s?c\.?(?=\s|$)/g,'$1ac').replace(/\b(?:a|one) hundred\b/g,'100')
  .replace(NUM_TENS_RE,(m,tens,unit)=>String(20+10*NUM_TENS.indexOf(tens)+(unit?NUM_UNITS.indexOf(unit):0))).replace(NUM_UNITS_RE,m=>String(NUM_UNITS.indexOf(m))).replace(/\s+/g,' ').trim()}
const said=name=>digitsOf(String(name||'').toLowerCase());
/* “um set a c to twenty-seven” is “set ac to 27”. “ok” stays, because it is a key on the TV */
function spokenForm(t){return digitsOf(t.replace(/(^|\s)(?:umm*|uhh*m?|erm|hmm+)(?=[\s,.!?]|$)[,.]?/g,' ').replace(/^\s*(?:er|well|so),?\s+/,''))}
/* the words a computer answers to: its name, its short name, its target and its VM id. Nothing here knows a computer of its own */
const vmWords=v=>[said(v.name),said(v.short),v.key,'vm '+v.vmid,String(v.vmid)].filter(w=>w&&w.length>1).map(escRe).join('|');
const hostSaid=()=>TGT.host?said(T.hostName):'';
const routerSaid=()=>TGT.router?said(T.routerName):'';
const speakerSaid=()=>D.speaker.target?said(D.speaker.name):'';
const wordRe=w=>w?'|'+escRe(w):'';
const VERBS=[[/^(start|boot|power on|bring up|wake)$/,'start'],[/^(stop|shut ?down|power off|halt|turn off)$/,'stop'],[/^(restart|reboot|reset|bounce)$/,'restart']];
/* what is said about Swifty's memory is for Swifty, never for a device: “remember that the bedroom is best at 22” sets nothing */
const ABOUT_MEMORY=/^(?:please )?(?:remember|don'?t forget|do not forget|note that|make a note|keep in mind|forget|stop remembering|what do you (?:remember|know about))\b/;
function parse(raw){
  const text=spokenForm(raw.trim().toLowerCase().replace(/[?!.,]+$/,''));if(!text||ABOUT_MEMORY.test(text))return {kind:'none'};let m;const owner=isOwner();
  const ownerOnly={kind:'none',why:'Only the owner can see the computers.'};const OPEN='^(?:open |show |show me |go to )?(?:the )?';
  const navRoom=ROOM_NAMES.find(r=>new RegExp(OPEN+escRe(said(r))+'$').test(text));if(navRoom)return {kind:'nav',to:'sheet:room:'+navRoom,label:'Open '+navRoom};
  /* a computer or the host, by the name the hub has for it */
  const navVm=VMS.find(v=>new RegExp(OPEN+'('+vmWords(v)+')$').test(text));if(navVm)return owner?{kind:'nav',to:'sheet:comp:'+navVm.id,label:'Open '+navVm.name}:{kind:'none',why:'That part is for the owner.'};
  if(hostSaid()&&new RegExp(OPEN+escRe(hostSaid())+'$').test(text))return owner?{kind:'nav',to:'sheet:host',label:'Open '+T.hostName}:{kind:'none',why:'That part is for the owner.'};
  /* places, by their names and by the words people actually try: console, crash, new light */
  if((m=text.match(/^(?:open|show|show me|go to)?\s*(?:the\s+)?(history|passkeys|technical|host|server|health|crashes|crash|add machine|rooms|wall(?: screen)?|more|lights|music|tv|pets|camera|printer|console|integrations?|plugins?|devices|find (?:new )?lights?|new lights?|add a light|living room|bedroom|office|kitchen|hallway)$/))){
    let k=m[1].replace(/ (screen)$/,'').replace(/^(integration|plugins?)$/,'integrations');if(/^(find (new )?lights?|new lights?|add a light)$/.test(k))k='findlights';
    const withConsole=VMS.find(v=>v.console&&!v.pending);
    const hs=TGT.host?'sheet:host':null;const map={history:'sheet:history',passkeys:'sheet:passkeys',technical:hs,host:hs,server:hs,health:hs,crashes:hs,crash:hs,'add machine':'sheet:addmachine',rooms:'view:rooms',wall:'wall',more:'sheet:more',lights:'sheet:lights',music:D.speaker.target||D.tv.target?'sheet:music':null,tv:D.tv.target?'sheet:music':null,pets:D.feeder.target||D.litter.target?'sheet:pets':null,camera:TGT.camera?'sheet:camera':null,printer:TGT.printer?'sheet:printer':null,console:withConsole?'sheet:comp:'+withConsole.id:null,integrations:'sheet:integrations',devices:'sheet:devices',findlights:'sheet:findlights'};
    const labels={health:'Open '+T.hostName+' at Health',crashes:'Open '+T.hostName+' at Health',crash:'Open '+T.hostName+' at Health',console:withConsole?'Open the '+withConsole.name+' console':'',findlights:'Find new lights',host:'Open '+T.hostName,server:'Open '+T.hostName,technical:'Open '+T.hostName};
    const room=ROOM_NAMES.find(r=>r.toLowerCase()===k);
    if(!room&&!map[k])return {kind:'none',why:k==='console'?'No computer here has a console.':'That is not here.'};
    if(!owner&&/^sheet:(history|passkeys|people|host|addmachine|comp:|findlights|printer|integrations|setup)/.test(map[k]||''))return {kind:'none',why:'That part is for the owner.'};
    return {kind:'nav',to:room?'sheet:room:'+room:map[k],label:labels[k]||'Open '+(room||m[1].replace(/^\w/,c=>c.toUpperCase())),jump:/^(health|crashes|crash)$/.test(k)?'health':k==='console'?'console':null}}
  if(/^lock$/.test(text))return {kind:'lock'};
  if(/(what|anything).*(wrong|need|attention)|^status$|^how is (the )?(house|home)$/.test(text)){const p=problems();return {kind:'query',answer:p.length?p.map(x=>x.text).join(' '):status().text}}
  /* a socket, an air conditioner or a light called by its own name: “turn off kettle”, “set bedroom ac to 22” */
  let dev=namedDevice(text);if(dev&&dev.kind!=='bulb'&&/\b(lights?|lamps?|bulbs?)\b/.test(text)&&!/\b(lights?|lamps?|bulbs?)\b/.test(dev.name))dev=null;
  if(dev&&dev.kind==='plug'){if(!owner)return {kind:'none',why:'Only the owner can switch the sockets.'};const p=dev.d,rest=text.replace(dev.name,' '),off=/\boff\b/.test(rest),on=/\bon\b/.test(rest)&&!off;
    if(on||off)return {kind:'action',a:ACT.plug(p.target,on)};if(/\btoggle\b/.test(rest)&&p.known)return {kind:'action',a:ACT.plug(p.target,!p.on)};return {kind:'query',answer:p.known?p.name+' is '+(p.on?'on.':'off.'):p.name+' is not reporting.'}}
  if((dev&&dev.kind==='ac')||AC_WORDS.test(text)){if(!owner)return {kind:'none',why:'Only the owner can use the air conditioner.'};if(!D.acs.length)return {kind:'none',why:'There is no air conditioner set up yet.'};
    const c=dev&&dev.kind==='ac'?dev.d:D.acs.length===1?D.acs[0]:null;if(!c)return {kind:'none',why:'Which one? '+D.acs.map(x=>x.name).join(' or ')+'.'};return acCommand(c,text.replace(said(c.name),' ').replace(AC_WORDS,' '))}
  /* verbs first, so "restart <a computer>" is an action on it and never a reading about it */
  if((m=text.match(/\b(start|boot|power on|bring up|wake|stop|shut ?down|power off|halt|restart|reboot|reset|bounce|turn off)\b/))){
    const verb=(VERBS.find(([re])=>re.test(m[1]))||[])[1];
    const vm=VMS.find(v=>new RegExp('\\b('+vmWords(v)+')\\b').test(text));
    const host=!!TGT.host&&new RegExp('\\b(host|server'+wordRe(hostSaid())+')\\b').test(text)&&!vm;const wg=!!TGT.router&&new RegExp('\\b(vpn|router|tunnel|remote access'+wordRe(routerSaid())+')\\b').test(text);const pr=!!TGT.printer&&/\b(print|printer)\b/.test(text);
    if(!owner&&(host||wg||pr||vm))return {kind:'none',why:'Only the owner can do that.'};
    if(host){if(verb==='start')return {kind:'none',why:T.hostKnown&&T.hostUp?T.hostName+' is already up. Nothing here can power it on from off.':T.hostName+' cannot be powered on from here. Someone has to press its button.'};return {kind:'action',a:ACT.host(verb==='restart'?'reboot':'shutdown'),where:'host'}}
    if(wg){if(verb!=='restart')return {kind:'none',why:'Remote access can only be restarted from here.'};return {kind:'action',a:ACT.wg(),where:TGT.host?'host':'router:'+TGT.router}}
    if(pr){if(verb==='start')return {kind:'action',a:ACT.printStart(),where:'printer'};return {kind:'none',why:'To stop the printer for good, say “emergency stop”. Say “pause the print” to pause.'}}
    if(vm&&!/\b(light|bulb|lamp|tv|television)\b/.test(text))return {kind:'action',a:ACT.vm(verb,vm.id),where:'comp:'+vm.id};
    if(/\b(computer|machine|vm)\b/.test(text)){const names=VMS.filter(v=>!v.pending).map(v=>v.short);return {kind:'none',why:names.length?'Which computer? '+(names.length>1?names.slice(0,-1).join(', ')+' or '+names[names.length-1]:names[0])+'.':'There are no computers here.'}}
  }
  /* readings about the computers belong to the owner */
  if(/(temp|hot|warm)/.test(text)&&new RegExp('(cpu|host|server|processor|temp'+wordRe(hostSaid())+')').test(text))return owner?{kind:'query',answer:T.pkgC==null&&T.pkgW==null?T.hostName+'’s temperature and power are not reporting.':`${T.hostName} is at ${T.pkgC==null?'an unknown temperature':T.pkgC.toFixed(0)+' °C'}, drawing ${T.pkgW==null?'an unknown power':T.pkgW.toFixed(0)+' W'}.`}:ownerOnly;
  if(/\b(memory|ram)\b/.test(text))return owner?{kind:'query',answer:T.memUsed==null?'Memory is not reporting.':`${(T.memUsed/1073741824).toFixed(1)} GB is in use.`}:ownerOnly;
  if(/\b(storage|disk|space)\b/.test(text))return owner?{kind:'query',answer:'How full the drives are is not reporting.'}:ownerOnly;
  if(/^(who|who is|who's) (signed|logged) in$/.test(text))return {kind:'query',answer:`${person().name}, with ${devName()}, since ${S.session?hhmm(new Date(S.session.created_at)):'just now'}.`};
  if(!owner&&/\b(print|printer|pause|resume|e-?stop|emergency)\b/.test(text))return {kind:'none',why:'Only the owner can use the printer.'};
  if(/\b(emergency stop|e-?stop)\b/.test(text))return {kind:'action',a:ACT.estop(),where:'printer'};
  if(/\bpause\b/.test(text)&&/\b(print|printer|it)\b|^pause$/.test(text)){if(T.printer.state!=='printing')return {kind:'none',why:'The print is '+T.printer.state+', so there is nothing to pause.'};return {kind:'action',a:ACT.printPause(),where:'printer'}}
  if(/\bresume\b/.test(text)&&(/\b(print|printer)\b/.test(text)||/^resume$/.test(text))){if(T.printer.state!=='paused')return {kind:'none',why:'The print is not paused.'};return {kind:'action',a:ACT.printPause(),where:'printer'}}
  if((m=text.match(/\bfeed\b(?:.*?(\d+)\s*g)?/)))return {kind:'action',a:ACT.feed(m[1]?+m[1]:10),where:'pets'};
  if(/\b(clean|litter|scoop|toilet)\b/.test(text))return {kind:'action',a:ACT.clean(),where:'pets'};
  if((m=text.match(/\b(evening|focus|movie|all off|everything off|lights off)\b/))&&!(m[1]==='lights off'&&ROOM_NAMES.some(r=>text.includes(said(r))))){const n=/off$/.test(m[1])?'Lights off':m[1][0].toUpperCase()+m[1].slice(1);return {kind:'scene',name:n}}
  if(/\b(tv|television)\b|\b(next|previous) channel\b|\bchannel (up|down)\b/.test(text)){if((m=text.match(/(?:hdmi|input)\s*([1-9])/)))return {kind:'action',a:ACT.tvInput('Input '+m[1])};if(/live tv/.test(text))return {kind:'action',a:ACT.tvInput('Live TV')};const key=tvKeyOf(text);if(key)return {kind:'action',a:ACT.tvKey(key[0],key[1])};if(/\boff\b/.test(text))return {kind:'action',a:ACT.tvPower(false),where:'music'};if(/\bon\b/.test(text))return {kind:'action',a:ACT.tvPower(true),where:'music'};return {kind:'query',answer:D.tv.known?('The TV is '+(D.tv.on?'on, showing '+D.tv.input:'off')+'.'):'The TV is not reporting.'}}
  if(/\b(mute|silence|quiet)\b/.test(text)&&!/unmute|quieter/.test(text))return {kind:'action',a:ACT.mute(true)};
  if(/\bunmute\b/.test(text))return {kind:'action',a:ACT.mute(false)};
  /* the speaker, by the words people use for one and by its own name */
  const spRe=new RegExp('\\b(speaker|sound|music|audio|songs?|playback'+wordRe(speakerSaid())+')\\b');
  if(/\bnight ?mode\b/.test(text)||/\bnight\b/.test(text)&&spRe.test(text))return {kind:'action',a:ACT.night(!/\boff\b/.test(text))};
  if(/\b(play|resume|pause|stop)\b/.test(text)&&spRe.test(text))return {kind:'action',a:ACT.play(/\b(play|resume)\b/.test(text))};
  const volKnown=D.speaker.known;const volWhy={kind:'none',why:'The speaker is not reporting, so its volume is not known. Say a number instead.'};
  if(/^(louder|turn it up|a bit louder)$/.test(text))return volKnown?{kind:'action',a:ACT.vol(D.speaker.vol+5)}:volWhy;
  if(/^(quieter|softer|turn it down|a bit quieter)$/.test(text))return volKnown?{kind:'action',a:ACT.vol(D.speaker.vol-5)}:volWhy;
  if(/\bvolume\b/.test(text)||spRe.test(text)){if((m=text.match(/(\d{1,3})/)))return {kind:'action',a:ACT.vol(+m[1])};if(/\b(up|louder|more)\b/.test(text))return volKnown?{kind:'action',a:ACT.vol(D.speaker.vol+5)}:volWhy;if(/\b(down|quieter|lower|less|softer)\b/.test(text))return volKnown?{kind:'action',a:ACT.vol(D.speaker.vol-5)}:volWhy;return {kind:'query',answer:volKnown?'The speaker is at '+D.speaker.vol+(D.speaker.mute?', muted':'')+'.':'The speaker is not reporting.'}}
  const roomSaid=ROOM_NAMES.find(r=>text.includes(said(r)));
  if(/\b(bulb|light|lamp)s?\b|\bdim\b|\bbright/.test(text)||roomSaid||(dev&&dev.kind==='bulb')){
    /* "set bulb 3 to 100": the light's number is taken out before the level is read */
    const one=text.match(/\b(?:bulb|light|lamp)\s*(\d+)\b/);const n=one?+one[1]:null;const rest=one?text.replace(one[0],' '):text;
    const pct=rest.match(/\b(?:to|at)\s*(\d{1,3})\s*(%|percent)?\b/)||rest.match(/\b(\d{1,3})\s*(%|percent)?\b/);let v=pct?+pct[1]:null;
    const colour=HUES.find(([c])=>new RegExp('\\b'+c+'\\b').test(text));const off=/\boff\b/.test(text),on=/\bon\b/.test(text)&&!off;
    /* a room named in the sentence limits the change to that room, and nothing else */
    if(roomSaid){const saved=((roomByName(roomSaid)||{}).scenes||[]).map(s=>s&&s.name||String(s)).find(n=>n&&!/^(on|off|bright)$/i.test(n)&&new RegExp('\\b'+escRe(said(n))+'\\b').test(text));
      if(saved||/\brelax(?:ed|ing)?\b/.test(text)){const name=saved||'Relax';const a=name==='Relax'&&!saved?ACT.roomLevel(roomSaid,35):ACT.roomScene(roomSaid,name);a.label=roomSaid+' scene '+name;return {kind:'action',a}}
      const bs=roomBulbs(roomSaid);if(!bs.length)return {kind:'none',why:'There are no lights in the '+roomSaid+' yet.'};const RL=lightsSummary(bs);
      if(colour)return {kind:'multi',list:bs.filter(b=>b.on).map(b=>ACT.bulbHue(b.id,colour[1],colour[0])),label:roomSaid+' lights to '+colour[0]};
      if(off)return {kind:'action',a:ACT.roomLevel(roomSaid,0)};if(v!=null)return {kind:'action',a:ACT.roomLevel(roomSaid,clamp(v,1,100))};if(/\bdim\b/.test(text))return {kind:'action',a:ACT.roomLevel(roomSaid,Math.max(5,(RL.avg||50)-20))};if(/\bbright/.test(text))return {kind:'action',a:ACT.roomLevel(roomSaid,Math.min(100,(RL.avg||40)+20))};if(on)return {kind:'action',a:ACT.roomLevel(roomSaid,RL.avg||60)};return {kind:'query',answer:roomSaid+': '+RL.state.toLowerCase()+(RL.n?' at '+RL.avg+'%.':'.')}}
    if(n&&!bulbById(n))return {kind:'none',why:'There is no light with that number. Say a room instead, like “dim the bedroom lights”.'};
    const named=D.bulbs.find(b=>text.includes(said(bulbName(b))));const id=n||(named?named.id:null);
    const L=lightsSummary();
    if(!id){if(off)return {kind:'action',a:ACT.lightsLevel(0)};if(v!=null)return {kind:'action',a:ACT.lightsLevel(clamp(v,1,100))};if(/\bdim\b/.test(text))return {kind:'action',a:ACT.lightsLevel(Math.max(5,(L.avg||50)-20))};if(/\bbright/.test(text))return {kind:'action',a:ACT.lightsLevel(Math.min(100,(L.avg||40)+20))};if(on)return {kind:'action',a:ACT.lightsLevel(L.avg||60)};return {kind:'query',answer:L.state+(L.n?' at '+L.avg+'%.':'.')}}
    const b=bulbById(id);if(colour)return {kind:'action',a:ACT.bulbHue(id,colour[1],colour[0])};if(off)return {kind:'action',a:ACT.bulbOn(id,false)};if(v!=null)return {kind:'action',a:ACT.bulbBri(id,clamp(v,1,100))};if(/\bdim\b/.test(text))return {kind:'action',a:ACT.bulbBri(id,Math.max(5,b.bri-20))};if(/\bbright/.test(text))return {kind:'action',a:ACT.bulbBri(id,Math.min(100,b.bri+20))};if(on)return {kind:'action',a:ACT.bulbOn(id,true)};return {kind:'query',answer:bulbName(b)+' is '+(b.on?'on at '+b.bri+'%.':'off.')}}
  return {kind:'none'};
}
/* what search knows: commands, and every place with the words people use for it */
const SUGG=[
  {q:'Movie',l:'Movie',k:'lights',icon:'bulb',al:'scene film'},{q:'Evening',l:'Evening',k:'lights',icon:'bulb',al:'scene'},{q:'Turn off the lights',l:'Turn off the lights',k:'lights',icon:'bulb'},{q:'Dim the lights',l:'Dim the lights',k:'lights',icon:'bulb'},{q:'Dim the bedroom lights',l:'Dim the bedroom lights',k:'one room',icon:'bulb'},
  {q:'Volume 20',l:'Volume 20',k:'speaker',icon:'speaker',al:'sound music'},{q:'Mute',l:'Mute',k:'speaker',icon:'speaker'},{q:'TV input 2',l:'TV to input 2',k:'TV',icon:'tv',al:'hdmi'},{q:'Feed the pets',l:'Feed the pets',k:'',icon:'paw',al:'cat dog feeder pet'},{q:'Clean the litter box',l:'Clean the litter box',k:'',icon:'paw',al:'toilet pet cat scoop'},
  {q:'Pause the print',l:'Pause the print',k:'',icon:'printer',owner:true},{q:'What is wrong?',l:'What is wrong?',k:'reading',icon:'warn',al:'status problem'},
  {q:'Open camera',l:'Camera',k:'',icon:'camera',al:'picture stream video'},{q:'Open rooms',l:'Rooms',k:'',icon:'rooms',al:'pin pinned living bedroom office kitchen hallway'},{q:'Open history',l:'History',k:'',icon:'history',owner:true,al:'log who asked audit'},{q:'Open passkeys',l:'Passkeys',k:'',icon:'key',owner:true,al:'devices sign in security'},
  {q:'Open devices',l:'Devices',k:'',icon:'tech',al:'device list all every kind'},{q:'Open integrations',l:'Integrations',k:'',icon:'puzzle',owner:true,al:'integration plugin plugins add setup set up remove'},
  {q:'Find new lights',l:'Find new lights',k:'',icon:'plus',owner:true,al:'new light add a light bulb bridge pair'},{q:'Open add machine',l:'Add machine',k:'',icon:'plus',owner:true,al:'computer agent host approve'},
  {q:'Open wall',l:'Wall screen',k:'',icon:'wall',al:'tablet'},{q:'Lock',l:'Lock',k:'',icon:'lock'}];
const EXAMPLES=['Movie','Dim the bedroom lights','Volume 20','Feed the pets'];
/* the host and each computer, by the names the hub has for them */
const placeSugg=()=>(TGT.host?[{q:'Open '+T.hostName,l:T.hostName,k:'',icon:'tech',owner:true,al:'host server crash crashes health memory temperature power'}]:[]).concat(VMS.filter(v=>!v.pending).map(v=>({q:'Open '+v.short,l:v.name,k:'',icon:v.icon,owner:true,al:'computer vm console'})),VMS.filter(v=>!v.pending).slice(0,1).map(v=>({q:'Restart '+v.short,l:'Restart the '+v.name,k:'passkey',icon:v.icon,owner:true,al:'reboot'})));
/* every named socket and air conditioner, so typing part of its name offers it */
const deviceSugg=()=>D.plugs.filter(p=>p.name!==p.target).map(p=>{const w=p.known&&p.on?'Turn off ':'Turn on ';return {q:w+p.name,l:w+p.name,k:'',icon:'power',owner:true,al:'socket switch plug'}}).concat(D.acs.filter(c=>c.name!==c.target).map(c=>({q:'Set '+c.name+' to 22',l:'Set '+c.name+' to 22°',k:'',icon:'snow',owner:true,al:'ac aircon air conditioner climate cool heat temperature'})));
/* the first result is what the sentence would do, so "restart" and a computer's name shows what will restart before you press Enter */
function resultRow(r,raw){
  if(r.kind==='nav')return {q:raw,l:r.label,k:'open',icon:'search'};
  if(r.kind==='scene')return {q:raw,l:r.name,k:'lights',icon:'bulb'};
  if(r.kind==='multi')return {q:raw,l:r.label.replace(/^\w/,c=>c.toUpperCase()),k:r.k!=null?r.k:'lights',icon:r.icon||'bulb'};
  if(r.kind==='action'){const a=r.a;return {q:raw,l:a.label,k:a.level==='critical'?'passkey':'',icon:a.cls==='light'?'bulb':a.cls==='audio'?'speaker':a.cls==='tv'?'tv':a.cls==='pet'?'paw':a.cls==='printer'?'printer':a.cls==='host'||a.cls==='device'?'tech':a.cls==='vm'?'pc':a.cls==='plug'?'power':a.cls==='ac'?'snow':'search'}}
  if(r.kind==='lock')return {q:raw,l:'Lock',k:'',icon:'lock'};
  return null;
}
function suggestions(raw){const q=raw.trim().toLowerCase();const all=SUGG.concat(placeSugg(),q?deviceSugg():[]).filter(s=>isOwner()||!s.owner);
  if(!q)return all.filter(s=>EXAMPLES.includes(s.q));
  const head=resultRow(parse(raw),raw.trim());const list=all.filter(s=>s.q.toLowerCase().includes(q)||s.l.toLowerCase().includes(q)||(s.al||'').split(' ').some(w=>w.startsWith(q))||q.split(' ').every(t=>t&&((s.al||'')+' '+s.l.toLowerCase()).includes(t)));
  const norm=s=>s.toLowerCase().replace(/\bthe\b/g,'').replace(/\s+/g,' ').trim();
  const out=head?[head]:[];for(const s of list){if(out.length>=7)break;if(out.some(o=>norm(o.l)===norm(s.l)))continue;out.push(s)}return out}
/* three-way rule: home things act, consequential ones open their confirm, critical ones stop at the sheet */
function execute(raw,ctx){
  const r=parse(raw);
  switch(r.kind){
    case 'nav':{if(!r.to)return false;if(S.wall&&!/^sheet:(lights|music|pets|room:)/.test(r.to)){toast('For that, use your phone.','');return false}if(!isOwner()&&/^sheet:(history|passkeys|people|host|addmachine|comp:|findlights|printer|integrations|setup)/.test(r.to)){toast('That part is for the owner.','');return false}if(r.to==='wall'){closeSheet();enterWall()}else if(r.to==='view:rooms'){closeSheet();setView('rooms')}else{SHEET.stack=[];openSheet(r.to.slice(6),null,{replace:true,jump:r.jump})}return true}
    case 'lock':closeSheet();lock();return true;
    case 'scene':if(!ctx.stay)closeSheet();runScene(r.name,ctx);return true;
    case 'multi':{if(!ctx.stay)closeSheet();if(!r.list.length){if(ctx.tell)ctx.tell('info','Those lights are off, so the colour stays.','multi');else toast('Those lights are off, so the colour stays.','');return true}
      /* one device's settings go one after another, so the second never overtakes the first */
      if(ctx.now){if(r.seq)(async()=>{for(const a of r.list)await doNow(a,ctx)})();else r.list.forEach((a,i)=>setTimeout(()=>doNow(a,ctx),i*120));return true}
      r.list.forEach((a,i)=>setTimeout(()=>dispatch(a,ctx),i*120));return true}
    case 'action':{const a=r.a;
      if(a.owner&&!isOwner()){toast('Only the owner can do that.','');return false}
      if(S.wall&&(a.cls==='vm'||a.cls==='host'||a.cls==='router'||a.cls==='printer'||a.cls==='key')){toast('For computers and settings, use your phone.','');return false}
      if(ctx.now&&a.level!=='critical'){if(!ctx.stay)closeSheet();doNow(a,ctx);return true}
      if(a.level==='plain'){closeSheet();dispatch(a,ctx);return true}
      if(a.disabled){toast(a.label+' is not possible right now. It is '+a.stillText+'.','');return false}
      SHEET.stack=[];openSheet(r.where,null,{replace:true,action:a,channel:ctx.channel,transcript:ctx.transcript});return true}
    default:return false;
  }
}

export {said,spokenForm,parse,suggestions,execute};
