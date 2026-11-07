/* csrc/rdma_shim.c - the few libibverbs calls nauti-fabric's RoCE data path needs, as plain functions.
 *
 * ibv_post_send() and ibv_poll_cq() are inline functions in <infiniband/verbs.h> that go through the provider's
 * ops table; Rust cannot call them without copying that struct's layout, which changes with rdma-core.  This file
 * is the one place that includes the header.  One reliable-connected queue pair per handle; the peer's queue pair
 * is named by the caller, who carried its numbers over the control link (data_quic's RdmaHello).
 */
#include <infiniband/verbs.h>

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

struct nauti_rdma {
    struct ibv_context* ctx;
    struct ibv_pd* pd;
    struct ibv_cq* cq;
    struct ibv_qp* qp;
    int port;
    int gid_index;
    uint32_t psn;
    uint16_t lid;
    int mtu;           /* enum ibv_mtu of the port */
    union ibv_gid gid;
};

static void say(char* err, size_t errlen, const char* what, int code) {
    if (!err || errlen == 0) return;
    if (code) snprintf(err, errlen, "%s: %s", what, strerror(code));
    else snprintf(err, errlen, "%s", what);
}

struct nauti_rdma* nauti_rdma_open(const char* dev, int port, int gid_index, char* err, size_t errlen) {
    int n = 0;
    struct ibv_device** list = ibv_get_device_list(&n);
    if (!list) {
        say(err, errlen, "ibv_get_device_list", errno);
        return NULL;
    }
    struct ibv_device* found = NULL;
    for (int i = 0; i < n; ++i)
        if (strcmp(ibv_get_device_name(list[i]), dev) == 0) found = list[i];
    if (!found) {
        snprintf(err, errlen, "no RDMA device %s (%d seen)", dev, n);
        ibv_free_device_list(list);
        return NULL;
    }
    struct nauti_rdma* h = calloc(1, sizeof(*h));
    h->port = port;
    h->gid_index = gid_index;
    h->ctx = ibv_open_device(found);
    ibv_free_device_list(list);
    if (!h->ctx) {
        say(err, errlen, "ibv_open_device", errno);
        free(h);
        return NULL;
    }
    struct ibv_port_attr pa;
    if (ibv_query_port(h->ctx, (uint8_t) port, &pa)) {
        say(err, errlen, "ibv_query_port", errno);
        goto fail;
    }
    if (pa.state != IBV_PORT_ACTIVE) {
        snprintf(err, errlen, "%s port %d is not active (state %d)", dev, port, (int) pa.state);
        goto fail;
    }
    h->lid = pa.lid;
    h->mtu = pa.active_mtu;
    if (ibv_query_gid(h->ctx, (uint8_t) port, gid_index, &h->gid)) {
        say(err, errlen, "ibv_query_gid", errno);
        goto fail;
    }
    h->pd = ibv_alloc_pd(h->ctx);
    if (!h->pd) {
        say(err, errlen, "ibv_alloc_pd", errno);
        goto fail;
    }
    h->cq = ibv_create_cq(h->ctx, 128, NULL, NULL, 0);
    if (!h->cq) {
        say(err, errlen, "ibv_create_cq", errno);
        goto fail;
    }
    struct ibv_qp_init_attr qa;
    memset(&qa, 0, sizeof(qa));
    qa.send_cq = h->cq;
    qa.recv_cq = h->cq;
    qa.cap.max_send_wr = 64;
    qa.cap.max_recv_wr = 4;
    qa.cap.max_send_sge = 1;
    qa.cap.max_recv_sge = 1;
    qa.qp_type = IBV_QPT_RC;
    h->qp = ibv_create_qp(h->pd, &qa);
    if (!h->qp) {
        say(err, errlen, "ibv_create_qp", errno);
        goto fail;
    }
    struct ibv_qp_attr a;
    memset(&a, 0, sizeof(a));
    a.qp_state = IBV_QPS_INIT;
    a.pkey_index = 0;
    a.port_num = (uint8_t) port;
    a.qp_access_flags = IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_READ | IBV_ACCESS_REMOTE_WRITE;
    int rc = ibv_modify_qp(h->qp, &a, IBV_QP_STATE | IBV_QP_PKEY_INDEX | IBV_QP_PORT | IBV_QP_ACCESS_FLAGS);
    if (rc) {
        say(err, errlen, "ibv_modify_qp INIT", rc);
        goto fail;
    }
    srand48((long) time(NULL) ^ (long) h->qp->qp_num);
    h->psn = (uint32_t) lrand48() & 0xffffff;
    return h;
fail:
    if (h->qp) ibv_destroy_qp(h->qp);
    if (h->cq) ibv_destroy_cq(h->cq);
    if (h->pd) ibv_dealloc_pd(h->pd);
    ibv_close_device(h->ctx);
    free(h);
    return NULL;
}

/* Registers [addr, addr + len).  remote_read: a peer may RDMA READ it (a read-only mapping takes this alone);
 * else it is this side's own buffer (local write).  Returns the ibv_mr, or NULL with the reason. */
void* nauti_rdma_reg(struct nauti_rdma* h, void* addr, size_t len, int remote_read, uint32_t* lkey, uint32_t* rkey,
                     char* err, size_t errlen) {
    int access = remote_read ? IBV_ACCESS_REMOTE_READ : IBV_ACCESS_LOCAL_WRITE;
    struct ibv_mr* mr = ibv_reg_mr(h->pd, addr, len, access);
    if (!mr) {
        say(err, errlen, "ibv_reg_mr", errno);
        return NULL;
    }
    *lkey = mr->lkey;
    *rkey = mr->rkey;
    return mr;
}

int nauti_rdma_dereg(void* mr) {
    return mr ? ibv_dereg_mr((struct ibv_mr*) mr) : 0;
}

void nauti_rdma_local(struct nauti_rdma* h, uint32_t* qpn, uint32_t* psn, uint8_t gid[16], uint16_t* lid, int* mtu) {
    *qpn = h->qp->qp_num;
    *psn = h->psn;
    memcpy(gid, h->gid.raw, 16);
    *lid = h->lid;
    *mtu = h->mtu;
}

/* Connects this queue pair to the peer's (its numbers from the control link) and brings it to RTS. */
int nauti_rdma_connect(struct nauti_rdma* h, uint32_t qpn, uint32_t psn, const uint8_t gid[16], uint16_t lid,
                       int peer_mtu, char* err, size_t errlen) {
    struct ibv_qp_attr a;
    memset(&a, 0, sizeof(a));
    a.qp_state = IBV_QPS_RTR;
    a.path_mtu = (enum ibv_mtu) (peer_mtu < h->mtu ? peer_mtu : h->mtu);
    a.dest_qp_num = qpn;
    a.rq_psn = psn;
    a.max_dest_rd_atomic = 16;
    a.min_rnr_timer = 12;
    a.ah_attr.is_global = 1;
    memcpy(a.ah_attr.grh.dgid.raw, gid, 16);
    a.ah_attr.grh.sgid_index = (uint8_t) h->gid_index;
    a.ah_attr.grh.hop_limit = 64;
    a.ah_attr.grh.traffic_class = 0;
    a.ah_attr.dlid = lid;
    a.ah_attr.sl = 0;
    a.ah_attr.src_path_bits = 0;
    a.ah_attr.port_num = (uint8_t) h->port;
    int rc = ibv_modify_qp(h->qp, &a, IBV_QP_STATE | IBV_QP_AV | IBV_QP_PATH_MTU | IBV_QP_DEST_QPN | IBV_QP_RQ_PSN |
                                      IBV_QP_MAX_DEST_RD_ATOMIC | IBV_QP_MIN_RNR_TIMER);
    if (rc) {
        say(err, errlen, "ibv_modify_qp RTR", rc);
        return -1;
    }
    memset(&a, 0, sizeof(a));
    a.qp_state = IBV_QPS_RTS;
    a.timeout = 14;
    a.retry_cnt = 7;
    a.rnr_retry = 7;
    a.sq_psn = h->psn;
    a.max_rd_atomic = 16;
    rc = ibv_modify_qp(h->qp, &a, IBV_QP_STATE | IBV_QP_TIMEOUT | IBV_QP_RETRY_CNT | IBV_QP_RNR_RETRY | IBV_QP_SQ_PSN |
                                  IBV_QP_MAX_QP_RD_ATOMIC);
    if (rc) {
        say(err, errlen, "ibv_modify_qp RTS", rc);
        return -1;
    }
    return 0;
}

static double now_s(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double) t.tv_sec + (double) t.tv_nsec * 1e-9;
}

/* One RDMA READ of len bytes from the peer's registered memory into this side's, waited for. */
int nauti_rdma_read(struct nauti_rdma* h, void* local, uint32_t lkey, uint64_t remote, uint32_t rkey, uint32_t len,
                    int timeout_ms, char* err, size_t errlen) {
    struct ibv_sge sge = {.addr = (uintptr_t) local, .length = len, .lkey = lkey};
    struct ibv_send_wr wr, *bad = NULL;
    memset(&wr, 0, sizeof(wr));
    wr.wr_id = 1;
    wr.sg_list = &sge;
    wr.num_sge = 1;
    wr.opcode = IBV_WR_RDMA_READ;
    wr.send_flags = IBV_SEND_SIGNALED;
    wr.wr.rdma.remote_addr = remote;
    wr.wr.rdma.rkey = rkey;
    int rc = ibv_post_send(h->qp, &wr, &bad);
    if (rc) {
        say(err, errlen, "ibv_post_send", rc);
        return -1;
    }
    const double until = now_s() + timeout_ms / 1000.0;
    struct ibv_wc wc;
    for (;;) {
        int n = ibv_poll_cq(h->cq, 1, &wc);
        if (n < 0) {
            say(err, errlen, "ibv_poll_cq", errno);
            return -1;
        }
        if (n == 1) break;
        if (now_s() > until) {
            snprintf(err, errlen, "RDMA READ of %u bytes: no completion within %d ms", len, timeout_ms);
            return -1;
        }
    }
    if (wc.status != IBV_WC_SUCCESS) {
        snprintf(err, errlen, "RDMA READ: %s (%d)", ibv_wc_status_str(wc.status), (int) wc.status);
        return -1;
    }
    return 0;
}

void nauti_rdma_close(struct nauti_rdma* h) {
    if (!h) return;
    if (h->qp) ibv_destroy_qp(h->qp);
    if (h->cq) ibv_destroy_cq(h->cq);
    if (h->pd) ibv_dealloc_pd(h->pd);
    if (h->ctx) ibv_close_device(h->ctx);
    free(h);
}
