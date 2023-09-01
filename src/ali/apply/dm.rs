use crate::ali::Dm;
use crate::errors::AliError;
use crate::linux;
use crate::run::apply::Action;

pub fn apply_dms(dms: &[Dm]) -> Result<Vec<Action>, AliError> {
    let mut actions = Vec::new();

    for dm in dms {
        let result = apply_dm(dm);
        if let Err(err) = result {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(Action::ApplyDm),
                actions_performed: actions,
            });
        }

        actions.extend(result.unwrap());
    }

    Ok(actions)
}

pub fn apply_dm(dm: &Dm) -> Result<Vec<Action>, AliError> {
    match dm {
        Dm::Luks(_) => Err(AliError::NotImplemented),
        Dm::Lvm(lvm) => {
            let mut actions = Vec::new();

            if let Some(pvs) = &lvm.pvs {
                for pv in pvs {
                    let action_create_pv = Action::CreateDmLvmPv(pv.clone());

                    linux::lvm::create_pv(pv)?;
                    actions.push(action_create_pv);
                }
            }

            if let Some(vgs) = &lvm.vgs {
                for vg in vgs {
                    let vg_name = format!("/dev/{}", vg.name);
                    let action_create_vg = Action::CreateDmLvmVg {
                        pvs: vg.pvs.clone(),
                        vg: vg_name.clone(),
                    };

                    linux::lvm::create_vg(vg)?;
                    actions.push(action_create_vg);
                }
            }

            if let Some(lvs) = &lvm.lvs {
                for lv in lvs {
                    let vg_name = format!("/dev/{}", lv.vg);
                    let lv_name = format!("{vg_name}/{}", lv.name);
                    let action_create_lv = Action::CreateDmLvmLv {
                        vg: vg_name.clone(),
                        lv: lv_name.clone(),
                    };

                    linux::lvm::create_lv(lv)?;
                    actions.push(action_create_lv);
                }
            }

            Ok(actions)
        }
    }
}