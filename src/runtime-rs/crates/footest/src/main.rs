use std::fs::OpenOptions;
use std::os::fd::IntoRawFd;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use anyhow::{anyhow, Context, Result};

use log::info;

use vmm_sys_util::eventfd::EventFd;

use dragonball::api::v1::{
    BootSourceConfig, InstanceInfo, VmmAction, VmmRequest, VmmResponse, VmmService,
};
use dragonball::vm::VmConfigInfo;
use dragonball::Vmm;

const KVM_DEVICE: &str = "/dev/kvm";
const DRAGONBALL_VERSION: &str = "0.1.0";

fn main() {
    env_logger::init();
    let id: &str = "123456";
    let kvm = OpenOptions::new()
        .read(true)
        .write(true)
        .open(KVM_DEVICE)
        .unwrap();
    info!("{:?}", kvm);

    let seccomp = vec![];
    let vmm_shared_info = Arc::new(RwLock::new(InstanceInfo::new(
        String::from(id),
        DRAGONBALL_VERSION.to_string(),
    )));

    let to_vmm_fd = EventFd::new(libc::EFD_NONBLOCK)
        .unwrap_or_else(|_| panic!("Failed to create eventfd for vmm"));
    let api_event_fd2 = to_vmm_fd.try_clone().expect("Failed to dup eventfd");

    let (to_vmm, from_runtime) = channel();
    let (to_runtime, from_vmm) = channel();

    let vmm_service = VmmService::new(from_runtime, to_runtime);

    let to_vmm: Option<Sender<VmmRequest>> = Some(to_vmm);
    let from_vmm: Option<Receiver<VmmResponse>> = Some(from_vmm);

    info!("before Vmm::new");
    let vmm = Vmm::new(
        vmm_shared_info,
        api_event_fd2,
        seccomp.clone(),
        seccomp.clone(),
        Some(kvm.into_raw_fd()),
    )
    .expect("Failed to start vmm");

    info!("{:?} {:?}", to_vmm, from_vmm);

    let vmm_thread = thread::Builder::new()
        .name("vmm_master".to_owned())
        .spawn(move || {
            || -> Result<i32> {
                let exit_code = Vmm::run_vmm_event_loop(Arc::new(Mutex::new(vmm)), vmm_service);
                Ok(exit_code)
            }()
            .unwrap();
        })
        .expect("Failed to start vmm event loop");
    info!("{:?}", vmm_thread);

    let vm_config = VmConfigInfo {
        serial_path: Some(String::from("/tmp/console.sock")),
        mem_size_mib: 512,
        vcpu_count: 1,
        max_vcpu_count: 1,
        mem_type: String::from("shmem"),
        mem_file_path: String::from(""),
        ..Default::default()
    };
    info!("{:?}", vm_config);

    let action = VmmAction::SetVmConfiguration(vm_config);
    // match send_request(&to_vmm, &from_vmm, &to_vmm_fd, action) {
    //     Ok(vmm_outcome) => match *vmm_outcome {
    //         Ok(vmm_data) => Ok(vmm_data),
    //         Err(vmm_action_error) => Err(anyhow!("vmm action error: {:?}", vmm_action_error)),
    //     },
    //     Err(e) => Err(e),
    // }
    // .unwrap();
    handle_request(&to_vmm, &from_vmm, &to_vmm_fd, action);

    let config = BootSourceConfig {
        kernel_path: String::from("/opt/kata/share/kata-containers/vmlinux-5.19.2-96"),
        initrd_path: Some(String::from("/root/datas/centos-no-kernel-initramfs.img")),
        boot_args: Some(String::from("console=ttyS0 reboot=k panic=1 pci=off")),
    };
    let action = VmmAction::ConfigureBootSource(config);
    handle_request(&to_vmm, &from_vmm, &to_vmm_fd, action);

    handle_request(&to_vmm, &from_vmm, &to_vmm_fd, VmmAction::StartMicroVm);

    vmm_thread.join().unwrap();
}

fn handle_request(
    to_vmm: &Option<Sender<VmmRequest>>,
    from_vmm: &Option<Receiver<VmmResponse>>,
    to_vmm_fd: &EventFd,
    vmm_action: VmmAction,
) {
    match send_request(&to_vmm, &from_vmm, &to_vmm_fd, vmm_action) {
        Ok(vmm_outcome) => match *vmm_outcome {
            Ok(vmm_data) => Ok(vmm_data),
            Err(vmm_action_error) => Err(anyhow!("vmm action error: {:?}", vmm_action_error)),
        },
        Err(e) => Err(e),
    }
    .unwrap();
}

fn send_request(
    to_vmm: &Option<Sender<VmmRequest>>,
    from_vmm: &Option<Receiver<VmmResponse>>,
    to_vmm_fd: &EventFd,
    vmm_action: VmmAction,
) -> Result<VmmResponse> {
    if let Some(ref to_vmm) = to_vmm {
        to_vmm
            .send(Box::new(vmm_action.clone()))
            .with_context(|| format!("Failed to send  {:?} via channel ", vmm_action))?;
    } else {
        return Err(anyhow!("to_vmm is None"));
    }

    //notify vmm action
    if let Err(e) = to_vmm_fd.write(1) {
        return Err(anyhow!("failed to notify vmm: {}", e));
    }

    if let Some(from_vmm) = from_vmm.as_ref() {
        match from_vmm.recv() {
            Err(e) => Err(anyhow!("vmm recv err: {}", e)),
            Ok(vmm_outcome) => Ok(vmm_outcome),
        }
    } else {
        Err(anyhow!("from_vmm is None"))
    }
}
