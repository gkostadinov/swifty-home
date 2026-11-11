import {TGT,stateOf,reasonOf,usable,DEVINFO,NR} from '../core/store.js';
import {deviceSheet} from './generic.js';

/* ============================================================ telemetry: a collector's latest readings, as it sends them. The host's page draws them as charts */
const telemetrySheets={
  telemetry(t,opts){t=t||TGT.telemetry;
    const lead={word(){if(!usable(t))return (DEVINFO[t]||{}).reason||'Not available';return stateOf(t)?'Reporting':(reasonOf(t)||NR)},
      why(){return t===TGT.telemetry?'The host’s health and charts come from these readings.':''}};
    return deviceSheet(t,opts,lead);
  },
};

export {telemetrySheets};
