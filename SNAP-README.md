# OpenShell Snap

This branch is a PoC of a simpler, more secure and more easily managed OpenShell install and distribution experience.

The goal is this UX is to make it trivial for any user to get OpenShell and start working with it locally using
VM sandboxes.

If they want, they can also install a local K8s and registry, deploy the gateway and run sandboxes on that. The same
commands work with a remote Kubernetes cluster.

The snap can use Podman locally too.

## Get started with LXD VM sandboxes:

LXD provides a higher-fidelity VM experience using QEMU-backed virtual machines. It requires
the LXD snap to be installed and initialized.

```
sudo snap install lxd
sudo lxd init                              # Accept defaults for ZFS-backed storage
sudo snap install openshell
sudo snap connect openshell:lxd            # Grant access to the LXD socket
```

Configure the gateway to use the LXD driver:

```
sudo snap set openshell driver=lxd
```

The gateway will restart automatically. Create a sandbox:

```
openshell sandbox create
```

## Get started with local MicroVM sandboxes:

```
sudo snap install openshell
openshell create sandbox
```

That's it. Nothing else needed. You are now able to create VM sandboxes locally.

## Add a K8s cluster as a place to run sandboxes

```
cat kubeconfig.conf | openshell gateway deploy k8s company-agents --registry ocireg.company.com:5000 --kubeconfig -
```

This will verify both the registy and the Kubernetes credential, push the containers to the registry, 

```
sudo snap install k8s --classic
sudo snap install registry

sudo k8s bootstrap
sudo k8s status --wait-ready

openshell cluster init --registry localhost:5000
openshell sandbox create
```

This could be a `curl | bash` script but we don't publish those as a recommended practice :)

## Build it

You need `rockcraft` and `snapcraft` to build the Docker image and snap respectively.

```
sudo snap install snapcraft --classic
sudo snap install rockcraft --classic
```


This will build a rock (docker image) and then a snap which bundles that rock. Bundling the rock is a short term thing,
we will shortly be able to attach the rock as a resource for the snap, which can be pulled dynamically (and updated
independently).

  `./tasks/scripts/build-snap.sh`

I think Gemini also integrated build:snap into the mise tooling but I don't know mise so haven't tried to exercise that.

You should now see `openshell_<version>_<architecture>.snap` in the top-level directory. Since it has just been built
locally it is not signed, so Ubuntu will not trust it by default. You need to install it with the `--dangerous` flag to
acknowledge that:

```
sudo snap install ./openshell_<v>_<arch>.snap --dangerous
sudo snap connect openshell:kube-config
```

All of this would become `sudo snap install openshell` for a user once openshell is published in the store.

## Use it

You need K8s:

```
sudo snap install k8s --classic --channel=1.35-classic
sudo k8s bootstrap
sudo k8s status --wait-ready
mkdir $HOME/.kube
sudo cp /etc/kubernetes/admin.conf $HOME/.kube/config
sudo chown $USER:$USER .kube/config
```

You should see a clean running k8s. If you want, test it out:

```
sudo snap install kubectl --classic --channel=1.35
kubectl get nodes
```

You need a Docker image registry:

```
sudo snap install registry
```

The registry should now be running on localhost:5000, feel free to test it with your fave OCI tools.

Now you need to initialize the cluster. This puts a daemonset on the k8s which installs a custom-written AppArmor profile
for the supervisor container. That gives the supervisor (and only the supervisor) the permissions it needs to setup
the sandboxes.

```
openshell cluster init --registry=localhost:5000
```

You should see a successful initialization. In theory this would work against a different registry, but you would also
need to configure your K8s to trust that registry. We're using a localhost K8s and a localhost registry in this walkthrough
so it should Just Work.

Now you can make a sandbox:

```
openshell sandbox create
```

... should drop you into a sandbox running on k8s.



# TODO

 - tests
 - figure out how best to provide certificates
 - auto-connect openshell:kvm ?
 - slim down rootfs - chiselled rock?
 - figure out if we still need kube-config
 - properly rmeove K3s
 - merge `openshell cluster init` and `openshell podman init` into `openshell-setup` that takes all the right flavour-specific parameters
 - chisel the k8s openshell-core rock
 - rename openshell.server to openshell.gateway
 - perhaps `openshell k8s init` would be more accurate now with Podman included in tree
 - figure out if podman socket can be made visible to gateway
 - do better than process-control
 - documentation!


# Developer Setup

```
git clone...
cd OpenShell

sudo snap install lxd                            # Used for clean image builds
sudo snap install rockcraft --classic            # Used for OCI and rootfs builds, in clean LXC containers
sudo snap install snapcraft --classic            # Used for snap builds, in clean LXC containers
sudo apt install umoci fakeroot

sudo lxd init                                    # Say yes to the defaults, it will give you a ZFS backed cache for developer iteration
```

## First build

```
./tasks/scripts/vm/build-rockcraft-rootfs.sh     # This will build the sandbox VM rootfs using rockcraft
./tasks/scripts/build-rock.sh                    # This will build the OCI used for gateway and supervisor
./tasks/scripts/build-snap.sh                    # THis will build the snap; it needs both the VM rootfs and the rock above
```

## Install developer build

```
sudo snap install ./openshell_0+git.*_<arch>.snap --dangerous
```

Now you should be able to:


## Sanity check

You want to be using the ZFS storage driver for LXC for faster build iteration:

```
$ lxc storage list
+---------+--------+--------------------------------------------+-------------+---------+---------+
|  NAME   | DRIVER |                   SOURCE                   | DESCRIPTION | USED BY |  STATE  |
+---------+--------+--------------------------------------------+-------------+---------+---------+
| default | zfs    | /var/snap/lxd/common/lxd/disks/default.img |             | 5       | CREATED |
+---------+--------+--------------------------------------------+-------------+---------+---------+
```


# WALKTHROUGH

sudo snap install openshell
openshell gateway list
openshell sandbox list
openshell sandbox create           # should error without KVM unless we get auto-connect
sudo snap connect openshell:kvm
openshell sandbox create           # should now succeed
openshell sandbox list

