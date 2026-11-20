`action.local.set` `keep: true` on a `local://` entity (decision `element-roles-and-reuse`): the device
keeps the fields it sets across starts of the app, per pod, and puts them back before the first frame. For
what belongs to one device rather than to the pod's data: the tab that is open, a filter, how a list is
sorted. It works offline and is not sent to the pod or to the user's other devices; a device that leaves
the pod forgets it. At most 200 values of 1 KB.
